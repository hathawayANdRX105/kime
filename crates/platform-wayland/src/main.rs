//! input-method-v2 平台壳：绑 seat → get_input_method → grab → engine。
//! 未消费的键经 zwp_virtual_keyboard_v1 原样送回，避免 grab 吞掉回车/快捷键。

use std::env;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc::{channel, Receiver, Sender};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::llm::{Debouncer, LlmClient};
use kime_core::ClipStore;
use kime_core::{Engine, Outcome};
use platform_wayland::clipboard_watch::spawn as spawn_clipboard_watcher;
use platform_wayland::context_batch::{plan_context_commit, ContextCommit, CONTEXT_TAIL_CHARS};
use platform_wayland::keyboard::{Keyboard, KEYMAP_FORMAT_XKB_V1};
use platform_wayland::mode_badge::{badge_enabled, BadgeFrame, BadgeSlot, ModeBadge};
use platform_wayland::repeat::{KeyRepeat, ShiftComposer, ShiftRelease};
use platform_wayland::route::{
    clip_route, key_log_line, route_press, route_release, shell_key, shift_holds_passthrough,
    ClipAction, PressAction,
};
use platform_wayland::tray::TrayIconManager;
use platform_wayland::SwallowTracker;
use platform_wayland::{Layout, Renderer};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_keyboard::KeyState,
        wl_region::WlRegion,
        wl_registry::{Event as RegistryEvent, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::WlShm,
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{
        Event as ZwpInputMethodKeyboardGrabEvent, ZwpInputMethodKeyboardGrabV2,
    },
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{Event as ZwpInputMethodEvent, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{Event as ZwpInputPopupSurfaceEvent, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Event as ZwlrLayerSurfaceEvent, ZwlrLayerSurfaceV1,
};

fn log(msg: &str) {
    eprintln!("[kime-ime] {msg}");
}

/// evdev keycodes — wayland 原生即此值，平台壳无需翻译（route.rs 同源约定）。
const KEY_ESC: u32 = 1;

/// 协议枚举值回退到裸 u32：合成器可能发协议未定义的值（WEnum::Unknown），
/// 一律当原始数字看，不因枚举缺失丢事件。
fn wenum_to_u32<T>(e: WEnum<T>) -> u32
where
    u32: From<T>,
{
    match e {
        WEnum::Value(v) => u32::from(v),
        WEnum::Unknown(v) => v,
    }
}

/// 逐键路由日志（第五轮「真机不生效、单测全绿」的定罪材料）：每次进
/// engine_press 的 press 一行，固定格式
/// `key code=<u32> ch=<char|-> mods=<c?><s?><a?> -> <Outcome>`，追加写
/// /tmp/kime-ime.log。人打字 ≤10 键/s、长按满速重复 ~30 行/s，不引日志
/// 框架；release 构建直接可见。写失败（只读/满盘）静默——日志不许卡键流。
///
/// 自截断：追加写发现文件已超 1MiB 就 set_len(0) 归零（保留文件本身）。
/// seek(0) 在 O_APPEND 下无效——内核强制每次写落到当前文件尾，只有归零
/// 才能让后续行从头写起。无人清理的 tmpfs 日志否则会一直涨到撑满为止。
fn key_log(code: u32, ch: Option<char>, ctrl: bool, alt: bool, shift: bool, outcome: &Outcome) {
    const MAX_LOG_BYTES: u64 = 1024 * 1024;
    let line = key_log_line(code, ch, ctrl, alt, shift, outcome);
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/kime-ime.log")
    {
        if f.metadata()
            .map(|m| m.len() > MAX_LOG_BYTES)
            .unwrap_or(false)
        {
            let _ = f.set_len(0);
        }
        let _ = f.write_all(line.as_bytes());
    }
}

fn dup_fd(fd: impl AsFd) -> Option<OwnedFd> {
    let raw = unsafe { libc::dup(fd.as_fd().as_raw_fd()) };
    if raw < 0 {
        None
    } else {
        Some(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}
/// CLOCK_MONOTONIC 毫秒——与 wlroots 系合成器 key 事件 time 同一起点，
/// poll 超时、重复节奏、透传时间戳共用这一把表。
fn now_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

struct LlmWorker {
    runtime: tokio::runtime::Runtime,
    client: LlmClient,
    debouncer: Debouncer,
    result_sender: Sender<Vec<kime_core::dict::Candidate>>,
}

impl LlmWorker {
    fn new(
        endpoint: String,
        model: String,
        result_sender: Sender<Vec<kime_core::dict::Candidate>>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime");
        let client = LlmClient::new(endpoint, model);
        let debouncer = Debouncer::new(std::time::Duration::from_millis(200));
        Self {
            runtime,
            client,
            debouncer,
            result_sender,
        }
    }
    fn request(&self, syllables: Vec<String>, context: Option<String>) {
        let client = self.client.clone();
        let debouncer = self.debouncer.clone();
        let sender = self.result_sender.clone();
        self.runtime.spawn(async move {
            if debouncer.should_fire().await {
                match client.request_async(syllables, context).await {
                    Ok(candidates) => {
                        let _ = sender.send(candidates);
                    }
                    Err(e) => log(&format!("LLM error: {}", e)),
                }
            }
        });
    }
}

struct SeatBind {
    seat: WlSeat,
    name: Option<String>,
}

/// 一块 shm 画布：memfd 与其 mmap 必须是同源的（历史上渲染写 B、合成器读 A，
/// 候选窗永远空白的根因）。Drop 时 munmap+close；WlBuffer 代理丢弃即 destroy。
struct ShmBuffer {
    buffer: WlBuffer,
    fd: i32,
    ptr: *mut u8,
    len: usize,
    w: u32,
    h: u32,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// wl_buffer user data：候选条归还 buffer 后据此找回槽位。
///
/// 角标的 buffer 走的是 `mode_badge::BadgeSlot` 与另一份 `Dispatch<WlBuffer,
/// BadgeSlot>` 实现——归属由**类型**分派决定，不是靠枚举变体。两者记错的后果
/// 一样致命（复用合成器仍持有的 buffer → `wl_shm.create_pool invalid arguments`
/// 打死整条连接），而类型分派比"变体对不对"更难写错。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BufferSlot(u8);

/// 候选窗本体：zwp_input_popup_surface_v2 + 简单双缓冲。
///
/// mango 的摆位守卫读 `surface->mapped`（= attach 过 buffer 且 commit 过），
/// 所以 surface 一建好就提交 1×1 全透明首帧；此后隐藏同样 = 提交 1×1 透明帧。
/// surface/role 全程只建一次、绝不销毁——input-popup 的可见性归 IM active 管。
struct PopupCanvas {
    surface: WlSurface,
    _role: ZwpInputPopupSurfaceV2,
    shm: WlShm,
    buffers: [Option<ShmBuffer>; 2],
    free: [bool; 2],
    frame: Option<WlCallback>,
    /// 新内容因无空闲 buffer 未能上屏：release/frame 到达即补画
    dirty: bool,
    /// 待显示内容：当前页候选 + 页内高亮下标
    content: Vec<String>,
    highlight: usize,
    /// 当前中英模式：驱动候选条首项的常驻模式字，也参与 set_content 去重
    chinese: bool,
    /// 模式提示闪现：Some = 刚发生中英切换，画只含 中/英 的小窗；
    /// 下一次任意按键（Key 处理开头）清掉。
    chip: Option<bool>,
}

impl PopupCanvas {
    // renderer 由 AppState 持有并以 &mut 传进来：候选条与常驻角标共用同一个已
    // 预热的渲染器（AppState::renderer），单 FontSystem、单次 warmup。两个表面
    // 不会同时借用——单线程事件循环，谁先画谁借。
    fn new(
        im: &ZwpInputMethodV2,
        comp: &WlCompositor,
        shm: &WlShm,
        renderer: &mut Renderer,
        qh: &QueueHandle<AppState>,
    ) -> Self {
        let surface = comp.create_surface(qh, ());
        // 必须用当前这个 zwp_input_method_v2 实例建 popup（历史上绑错过 IME 对象）
        let _role = im.get_input_popup_surface(&surface, qh, ());
        let mut canvas = Self {
            surface,
            _role,
            shm: shm.clone(),
            buffers: [None, None],
            free: [true, true],
            frame: None,
            dirty: false,
            content: Vec::new(),
            highlight: 0,
            chinese: true,
            chip: None,
        };
        canvas.present(renderer, qh);
        canvas
    }

    fn set_content(
        &mut self,
        candidates: Vec<String>,
        highlight: usize,
        chinese: bool,
        renderer: &mut Renderer,
        qh: &QueueHandle<AppState>,
    ) {
        if self.content == candidates && self.highlight == highlight && self.chinese == chinese {
            return;
        }
        self.content = candidates;
        self.highlight = highlight;
        self.chinese = chinese;
        self.present(renderer, qh);
    }

    /// 中英切换瞬间：闪现一次只含 中/英 的小窗（工单第 1 条）。
    fn flash_chip(&mut self, chinese: bool, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        self.chip = Some(chinese);
        self.present(renderer, qh);
    }

    /// 下一次按键收掉闪现；没闪现时零成本。
    fn clear_chip(&mut self, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        if self.chip.take().is_some() {
            self.present(renderer, qh);
        }
    }

    /// 强制隐藏（deactivate 用）：候选与未清的闪现窗一并抹掉。
    ///
    /// 焦点抖动（ACTIVATE/DEACTIVATE 高频切换）时合成器挂起的 buffer release
    /// 不再到达，free[] 卡在 false 且与实际所有权脱节，下一次 present 复用旧
    /// pool → wl_shm.create_pool invalid arguments 致协议错误崩溃。deactivate
    /// 是唯一能确定「合成器不再持有我的 buffer」的时刻，重置槽位记账。
    fn hide(&mut self, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        let visible = self.chip.is_some() || !self.content.is_empty() || self.highlight != 0;
        self.chip = None;
        self.content.clear();
        self.highlight = 0;
        if visible {
            self.present(renderer, qh);
        }
        // 重置在 present 之后：隐藏帧要先画完，再让槽位回到「全部可用」。
        // buffer 对象保留（尺寸命中时复用），只重置归属记账。
        self.free = [true, true];
        self.frame = None;
        self.dirty = false;
    }

    fn on_release(&mut self, slot: usize, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        log(&format!("popup: buffer release slot={slot}"));
        if slot < 2 {
            self.free[slot] = true;
        }
        if self.dirty {
            self.present(renderer, qh);
        }
    }

    fn on_frame(&mut self, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        log("popup: frame 回调到达");
        self.frame = None;
        if self.dirty {
            self.present(renderer, qh);
        }
    }

    /// 布局 → 找空闲槽位 → 画 → attach + commit + frame 请求。
    fn present(&mut self, renderer: &mut Renderer, qh: &QueueHandle<AppState>) {
        self.dirty = false;
        let layout = match self.chip {
            Some(chinese) => renderer.chip_layout(chinese),
            None => renderer.layout(&self.content, self.chinese),
        };
        let Some(slot) = self.pick_slot(&layout) else {
            log("popup: 两槽位都被合成器占用，present 挂起（等 release/frame）");
            self.dirty = true;
            return;
        };
        let reuse = self.buffers[slot].as_ref().is_some_and(|b| {
            b.w == layout.width && b.h == layout.height && b.len == layout.pixel_len()
        });
        if !reuse {
            // 槽位里的旧 buffer 必已 release（free 才会走到这），换掉安全
            match Self::create_buffer(&self.shm, &layout, BufferSlot(slot as u8), qh) {
                Ok(b) => self.buffers[slot] = Some(b),
                Err(e) => {
                    log(&format!("popup buffer 分配失败: {e}"));
                    // 本帧没上屏：把 dirty 置回，等下次 frame 回调重新 present，
                    // 否则候选条静默停在旧内容（与无空闲槽位那条早退行为对齐）
                    self.dirty = true;
                    return;
                }
            }
        }
        {
            let b = self.buffers[slot].as_ref().unwrap();
            let pixels = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
            renderer.paint(&layout, self.highlight, pixels);
        }
        let b = self.buffers[slot].as_ref().unwrap();
        self.surface.attach(Some(&b.buffer), 0, 0);
        self.surface
            .damage(0, 0, layout.width as i32, layout.height as i32);
        self.surface.commit();
        self.free[slot] = false;
        self.frame = Some(self.surface.frame(qh, ()));
    }

    fn pick_slot(&self, layout: &Layout) -> Option<usize> {
        let mut fallback = None;
        for i in 0..2 {
            if !self.free[i] {
                continue;
            }
            if let Some(b) = &self.buffers[i] {
                if b.w == layout.width && b.h == layout.height {
                    return Some(i);
                }
            }
            fallback = Some(i);
        }
        fallback
    }

    /// 避坑 4：宽高均为正、stride = width*4、Format::Argb8888。
    fn create_buffer(
        shm: &WlShm,
        layout: &Layout,
        slot: BufferSlot,
        qh: &QueueHandle<AppState>,
    ) -> Result<ShmBuffer, &'static str> {
        let (w, h) = (layout.width, layout.height);
        if w == 0 || h == 0 || w > 8192 || h > 1024 {
            return Err("popup 尺寸非法");
        }
        let len = (w as usize) * (h as usize) * 4;
        let fd =
            unsafe { libc::memfd_create(b"kime-popup\0".as_ptr() as *const i8, libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err("memfd_create 失败");
        }
        if unsafe { libc::ftruncate(fd, len as libc::off_t) } < 0 {
            unsafe { libc::close(fd) };
            return Err("ftruncate 失败");
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return Err("mmap 失败");
        }
        let fd_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let pool = shm.create_pool(fd_borrowed, len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            qh,
            slot,
        );
        pool.destroy();
        Ok(ShmBuffer {
            buffer,
            fd,
            ptr: ptr as *mut u8,
            len,
            w,
            h,
        })
    }
}

struct AppState {
    conn: Option<Connection>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    vk: Option<ZwpVirtualKeyboardV1>,
    vk_keymap_ready: bool,
    seats: Vec<SeatBind>,
    target_seat: Option<String>,
    engine: Option<Engine>,
    should_exit: bool,
    im_serial: u32,
    tray: TrayIconManager,
    llm_worker: Option<LlmWorker>,
    llm_receiver: Option<Receiver<Vec<kime_core::dict::Candidate>>>,
    /// press/release 配对记账（见 SwallowTracker）：press 被引擎吃掉的键，release 也不转发，
    /// 避免半截按键；press 一旦转发给应用，记账必须销掉，release 同样转发。
    swallowed: SwallowTracker,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    /// 候选窗：候选条 + 一次性模式闪现；text_input_rectangle 只留日志
    popup: Option<PopupCanvas>,
    /// 预热好的渲染器：main() 在 wayland 连接之前构造（Renderer::new 内含字形冷
    /// 路径 warmup）。候选条与常驻角标**共用这一个**——角标只在模式翻转时重画，
    /// 而翻转就在按键路径上，另建一个渲染器等于把 ~110ms 的字体 fallback 扫描
    /// 重新砸回第一次 Shift。
    renderer: Renderer,
    /// zwlr_layer_shell_v1：合成器不支持时为 None，角标整条跳过（其余功能不受影响）
    layer_shell: Option<ZwlrLayerShellV1>,
    /// 常驻模式角标：layer-shell 表面，全程只建一次
    badge: Option<ModeBadge>,
    /// xkb 解码：keymap/modifiers 事件喂状态，Key 事件取字符（取代旧手写码表）
    keyboard: Keyboard,
    /// 组合内长按自动重复：合成器不向 IM grab 投递 repeat（libinput 吞 value=2；
    /// mango 自研重复只喂全局键位），到点由主循环 poll 超时唤醒后合成（见 repeat.rs）。
    repeat: KeyRepeat,
    /// Shift 点击=切换 / 按住期间打字=临时英文不改模式（rime ascii_composer 语义）。
    shift_gesture: ShiftComposer,
    /// input-method-v2 双缓冲的 pending 状态：事件暂存、done 提交。
    /// surrounding_text 的 cursor 是字节偏移；cause 缺省 0 = INPUT_METHOD。
    pending_surrounding: Option<(String, usize)>,
    pending_cause: u32,
    /// content_type 本阶段只记录（日志/存字段），不参与决策。
    /// 去重用的上次 content_type（合成器每次提交都回发，同值不记日志）
    content_type: Option<(u32, u32)>,
    pending_content_type: Option<(u32, u32)>,
    /// 合成器告知的文本输入区（surface-local）。摆位归 zwp_input_popup_surface_v2
    /// 的 role 与合成器，本字段是「真的被摆位了」的诊断证据，不参与布局。
    cursor_rect: Option<(i32, i32, i32, i32)>,
    /// 剪贴板候选（M16）：动态历史 + deskctl 预设
    clip: ClipStore,
    /// 剪贴板模式的候选游标；None = 不在剪贴板模式
    clip_pick: Option<usize>,
    /// 剪贴板监视线程 → 主循环的新文本
    clip_rx: Option<Receiver<String>>,
}

impl AppState {
    fn new(target_seat: Option<String>, renderer: Renderer) -> Self {
        Self {
            conn: None,
            input_method_manager: None,
            input_method: None,
            grab: None,
            vk_manager: None,
            vk: None,
            vk_keymap_ready: false,
            seats: Vec::new(),
            target_seat,
            engine: None,
            should_exit: false,
            im_serial: 0,
            tray: TrayIconManager::new(true),
            llm_worker: None,
            llm_receiver: None,
            swallowed: SwallowTracker::default(),
            compositor: None,
            shm: None,
            popup: None,
            renderer,
            layer_shell: None,
            badge: None,
            keyboard: Keyboard::new(),
            repeat: KeyRepeat::default(),
            pending_surrounding: None,
            pending_cause: 0,
            content_type: None,
            pending_content_type: None,
            cursor_rect: None,
            shift_gesture: ShiftComposer::default(),
            clip: {
                let mut store = ClipStore::new();
                // deskctl 预设（只读）：目录不存在 → 空集，未装 deskctl 是常态
                let dir = std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".config/deskctl/snippets"))
                    .unwrap_or_default();
                store.load_presets(&dir);
                store
            },
            clip_pick: None,
            clip_rx: {
                let (tx, rx) = channel();
                spawn_clipboard_watcher(tx);
                Some(rx)
            },
        }
    }

    fn pick_seat(&self) -> Option<&SeatBind> {
        if let Some(want) = &self.target_seat {
            self.seats
                .iter()
                .find(|s| s.name.as_deref() == Some(want.as_str()))
                .or_else(|| self.seats.first())
        } else {
            self.seats.first()
        }
    }

    fn try_bind_im(&mut self, qh: &QueueHandle<Self>) {
        if self.input_method.is_some() {
            return;
        }
        let Some(mgr) = self.input_method_manager.as_ref() else {
            return;
        };
        let Some(seat) = self.pick_seat() else {
            return;
        };
        let im = mgr.get_input_method(&seat.seat, qh, ());
        log("get_input_method issued");
        self.input_method = Some(im);
    }

    fn try_bind_vk(&mut self, qh: &QueueHandle<Self>) {
        if self.vk.is_some() {
            return;
        }
        let Some(mgr) = self.vk_manager.as_ref() else {
            return;
        };
        let Some(seat) = self.pick_seat() else {
            return;
        };
        let vk = mgr.create_virtual_keyboard(&seat.seat, qh, ());
        log("virtual keyboard created");
        self.vk = Some(vk);
    }

    fn forward_key(&self, time: u32, key: u32, pressed: bool) {
        let Some(vk) = &self.vk else {
            log("vk missing, cannot forward key");
            return;
        };
        if !self.vk_keymap_ready {
            log("vk keymap not ready, drop forward");
            return;
        }
        let state = if pressed { 1 } else { 0 };
        vk.key(time, key, state);
    }

    fn try_recv_llm(&mut self, qh: &QueueHandle<Self>) {
        let mut merged = false;
        if let Some(receiver) = &self.llm_receiver {
            if let Ok(candidates) = receiver.try_recv() {
                if let Some(engine) = &mut self.engine {
                    engine.merge_ai(candidates);
                    merged = true;
                }
            }
        }
        if merged {
            self.popup_show(qh);
        }
    }

    fn ensure_engine(&mut self) {
        if self.engine.is_some() {
            return;
        }
        let config = Config::load();
        let dict_path = config.dict_path.clone();
        match Dict::open(&dict_path) {
            Ok(dict) => {
                log(&format!("dict opened: {dict_path}"));
                let engine = Engine::new(dict, config.clone());
                self.tray.set_chinese(engine.chinese());
                self.engine = Some(engine);
                // 实时候补默认关（config.ai_realtime）：上下文分词质量优先，
                // 想要 LLM 实时候补得显式打开。endpoint 缺失同样不 spawn。
                let endpoint = config.ai_endpoint.clone().unwrap_or_default();
                if config.ai_realtime && !endpoint.is_empty() {
                    let (tx, rx) = channel();
                    let model = config.ai_model.clone();
                    self.llm_worker = Some(LlmWorker::new(endpoint, model, tx));
                    self.llm_receiver = Some(rx);
                }
            }
            Err(e) => log(&format!("failed to open dict {dict_path}: {e}")),
        }
    }

    /// 候选窗只建一次：当前 IM 实例 + wl_compositor + wl_shm 齐了就建 canvas，
    /// 建好即提交 1×1 全透明首帧（不 commit 的 surface 永远不 mapped，mango 不摆位）。
    fn ensure_popup(&mut self, im: &ZwpInputMethodV2, qh: &QueueHandle<Self>) {
        if self.popup.is_some() {
            return;
        }
        let (Some(comp), Some(shm)) = (self.compositor.clone(), self.shm.clone()) else {
            log("wl_compositor/wl_shm 未绑定，候选窗不可用");
            return;
        };
        // 渲染器留在 AppState 上不 take：候选条与角标共用它，构造 canvas 时借一下
        // 即可（见 PopupCanvas::new）。popup surface/role 全程只建一次。
        let canvas = PopupCanvas::new(im, &comp, &shm, &mut self.renderer, qh);
        self.popup = Some(canvas);
        log("input popup 候选窗已建（已提交 1×1 透明首帧）");
    }

    /// 常驻模式角标：layer-shell 表面，全程只建一次。
    ///
    /// 缺 zwlr_layer_shell_v1 的合成器（GNOME/Wayland 原生、X11）直接跳过——角标是
    /// 增强项，缺了不能拖累输入法本身。KIME_MODE_BADGE=0 同样跳过。
    fn ensure_badge(&mut self, qh: &QueueHandle<Self>) {
        if self.badge.is_some() {
            return;
        }
        if !badge_enabled() {
            log("KIME_MODE_BADGE 已关闭，不建常驻角标");
            return;
        }
        let (Some(shell), Some(comp), Some(shm)) = (
            self.layer_shell.clone(),
            self.compositor.clone(),
            self.shm.clone(),
        ) else {
            return;
        };
        self.badge = Some(ModeBadge::new(&shell, &comp, &shm, qh));
        log("常驻模式角标已建（等待合成器 configure）");
        // 建面即刻对齐一次真实模式。`ModeBadge::new` 的初值是 `chinese: true`，
        // 而引擎也是 `true` 起步，正常启动下两者天然一致、这里等价于空操作。
        // 但 global 到达顺序不利时（本函数幂等 + 缺项早退，谁最后到谁触发），角标
        // 可能在用户已经切到英文之后才建出来——那就会先画一个错的「中」，要等下一个
        // 键才被 `sync_mode_badge` 自愈。此刻还没 configure，`sync_mode_badge` 走的是
        // "只记账不 attach"那条分支，正好把真实模式存进 `ModeBadge`，首帧即正确。
        self.sync_mode_badge(qh);
    }

    /// 模式指示的单一入口：把 `engine.chinese()` 同时喂给托盘桩与常驻角标。
    ///
    /// 角标只在模式真的翻转时才重画——它常驻显示，每帧重画纯属浪费，且重画要走
    /// 按键路径。`set_chinese` 返回是否真变了，没变就不碰 buffer。
    fn sync_mode_badge(&mut self, qh: &QueueHandle<Self>) {
        let chinese = self.engine.as_ref().is_none_or(|e| e.chinese());
        self.tray.set_chinese(chinese);
        let Some(badge) = self.badge.as_mut() else {
            return;
        };
        // 角标尚未收到 configure：协议禁止此时 attach buffer，present 自身会挡。
        if !badge.is_configured() {
            badge.set_chinese(chinese);
            return;
        }
        if badge.set_chinese(chinese) {
            badge.present(&mut self.renderer, qh);
        }
    }

    /// 候选条渲染的单一入口：剪贴板模式画剪贴板候选（历史+预设），否则画引擎页。
    fn popup_show(&mut self, qh: &QueueHandle<AppState>) {
        let (page, hl) = if let Some(pick) = self.clip_pick {
            let cands: Vec<String> = self
                .clip
                .candidates()
                .into_iter()
                .map(|e| {
                    // 多行候选压成一行显示（渲染层是单行条）
                    e.text.replace('\n', "␤")
                })
                .collect();
            let n = cands.len();
            (cands, if n > 0 { pick.min(n - 1) } else { 0 })
        } else {
            match &self.engine {
                Some(engine) => {
                    let (start, ps) = (engine.highlight(), engine.page().1);
                    let all = engine.candidates();
                    let page = all[start.min(all.len())..(start + ps).min(all.len())]
                        .iter()
                        .map(|c| c.text.clone())
                        .collect();
                    (page, engine.highlight().saturating_sub(start))
                }
                None => (Vec::new(), 0),
            }
        };
        log(&format!("popup_show 候选={page:?} hl={hl}"));
        // 引擎读取必须在下面 popup 的可变借用之前：剪贴板分支不碰引擎，
        // 无条件读一次即可（空引擎退回中文默认）。
        let chinese = self.engine.as_ref().map_or(true, |e| e.chinese());
        if let Some(canvas) = self.popup.as_mut() {
            canvas.set_content(page, hl, chinese, &mut self.renderer, qh);
        }
    }

    /// 隐藏 = 1×1 全透明帧，绝不 destroy surface（可见性归 IM active 状态管）。
    /// 只在 deactivate 用；键流上的清空走 popup_show。
    fn popup_hide(&mut self, qh: &QueueHandle<AppState>) {
        if let Some(canvas) = self.popup.as_mut() {
            canvas.hide(&mut self.renderer, qh);
        }
    }
    /// 切走应用时作废在途组合：引擎组合 + 发往旧应用的 preedit 一起清。
    /// 引擎无公开清组合入口（`Engine::clear_composition` 私有），与 XIM 前端
    /// 同一手法：喂一个裸 Esc（引擎 Esc 路径 = 全量清组合，不动中英模式）。
    /// preedit 不同步拉平的话，旧应用输入框里留半截拼音，新应用一按键就接着打。
    fn clear_composition(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            let _ = engine.key(shell_key(KEY_ESC, None, false, false));
        }
        if let Some(im) = &self.input_method {
            im.set_preedit_string(String::new(), 0, 0);
            im.commit(self.im_serial);
        }
    }
    fn apply_consumed(&mut self, qh: &QueueHandle<Self>) {
        let (syllables, context) = {
            let Some(engine) = self.engine.as_ref() else {
                return;
            };
            let pe = engine.preedit().to_string();
            log(&format!("consumed preedit={pe}"));
            if let Some(im) = &self.input_method {
                let cursor = engine.preedit_cursor() as i32;
                im.set_preedit_string(pe.clone(), 0, cursor);
                im.commit(self.im_serial);
            }
            // 上下文尾巴随请求一起走：LLM 拿到光标前文才能分词消歧
            let context = engine.context_tail().map(str::to_string);
            (
                pe.split('\'').map(String::from).collect::<Vec<_>>(),
                context,
            )
        };
        // 引擎的不可变借用到此结束，才能借 &mut self 去同步模式指示
        self.sync_mode_badge(qh);
        self.popup_show(qh);
        if let Some(worker) = &self.llm_worker {
            worker.request(syllables, context);
        }
    }

    fn apply_commit(&mut self, text: String, qh: &QueueHandle<Self>) {
        log(&format!("commit {text}"));
        self.deliver_commit(text, qh);
    }

    /// 投递上屏（无日志）：剪贴板候选文本不落 /tmp/kime-ime.log（隐私——
    /// 日志可含任意复制内容）。
    fn deliver_commit(&mut self, text: String, qh: &QueueHandle<Self>) {
        if let Some(im) = &self.input_method {
            im.commit_string(text);
            im.set_preedit_string(String::new(), 0, 0);
            im.commit(self.im_serial);
        }
        // 走 popup_show 而非 hide：中文模式下候选已被引擎清空 → 自然回到隐藏帧；
        // 若这次提交同时翻转了中英模式（Shift 上屏原串），翻转检测在 Key 处理里闪窗。
        self.popup_show(qh);
    }

    /// 剪贴板模式按键执行层：决策全部在纯函数 [`clip_route`]（可离线单测），
    /// 这里只做副作用（重绘/提交/模式翻转）。
    ///
    fn clip_mode_key(&mut self, code: u32, qh: &QueueHandle<Self>) -> bool {
        let n = self.clip.candidates().len();
        let ctrl = self.keyboard.ctrl();
        let action = clip_route(self.clip_pick.is_some(), self.clip_pick, ctrl, code, n);
        let consumed = !matches!(action, ClipAction::Forward);
        if consumed {
            // release 配对记账：clip 模式吞掉的 press，其 release 也吞，
            // 否则应用收到无 press 的裸 release（947a3a3 同族幽灵事件）。
            self.swallowed.consume(code);
        }
        match action {
            ClipAction::Enter => {
                self.clip_pick = Some(0);
                self.popup_show(qh);
            }
            ClipAction::Exit => {
                self.clip_pick = None;
                self.popup_show(qh);
            }
            ClipAction::Move(next) => {
                self.clip_pick = Some(next);
                self.popup_show(qh);
            }
            ClipAction::Commit(idx) => {
                let text = self.clip.candidates().get(idx).map(|e| e.text.clone());
                self.clip_pick = None;
                match text {
                    // 剪贴板文本不进日志：走无日志投递路径
                    Some(text) => self.deliver_commit(text, qh),
                    None => self.popup_show(qh),
                }
            }
            ClipAction::Swallow => {}
            ClipAction::Forward => {
                // 不在模式或未知键：退出模式（若在）并转交引擎
                if self.clip_pick.take().is_some() {
                    self.popup_show(qh);
                }
            }
        }
        consumed
    }

    /// 引擎按键的唯一路径：真 press、Shift 手势裁决补交的 toggle、长按合成的
    /// tick（synthetic=true）都从这里进，消费/放行的记账只有一份。
    fn engine_press(&mut self, code: u32, time: u32, synthetic: bool, qh: &QueueHandle<Self>) {
        // 收剪贴板监视线程的新文本（不阻塞：线程异步推送）
        while let Ok(text) = self
            .clip_rx
            .as_ref()
            .map(|rx| rx.try_recv())
            .unwrap_or(Err(std::sync::mpsc::TryRecvError::Disconnected))
        {
            self.clip.push(&text, kime_core::clipboard::now_ms());
            // 剪贴板模式开着 → 候选列表实时刷新
            if self.clip_pick.is_some() {
                self.popup_show(qh);
            }
        }
        // 剪贴板模式优先：C-; 触发 / 导航 / 提交都在这里闭环，不进引擎。
        if self.clip_mode_key(code, qh) {
            let (ctrl, alt) = (self.keyboard.ctrl(), self.keyboard.alt());
            key_log(code, None, ctrl, alt, false, &Outcome::Consumed);
            return;
        }
        let ch = self.keyboard.key_char(code);
        let (ctrl, alt) = (self.keyboard.ctrl(), self.keyboard.alt());
        let mode_before = self.engine.as_ref().map_or(true, |e| e.chinese());
        let outcome = match self.engine.as_mut() {
            Some(engine) => engine.key(shell_key(code, ch, ctrl, alt)),
            None => {
                // 词库没开成：一切键放行，重复表整体作废，永不重新起表。
                self.repeat.clear();
                Outcome::Ignored
            }
        };
        key_log(code, ch, ctrl, alt, matches!(code, 42 | 54), &outcome);
        // 消费之后组合是否还在：重复只在「组合内编辑键」起表（契约）。
        let composing = self
            .engine
            .as_ref()
            .is_some_and(|e| !e.preedit().is_empty());
        let route = route_press(code, synthetic, composing, &outcome);
        if route.disarm {
            self.repeat.release(code);
        }
        if route.arm {
            self.repeat.arm(code, now_ms());
        }
        match route.action {
            PressAction::Consume => {
                self.swallowed.consume(code);
                match outcome {
                    Outcome::Commit(text) => self.apply_commit(text, qh),
                    _ => self.apply_consumed(qh),
                }
            }
            PressAction::Forward => {
                // 同一个键可能从「被消费」中途变成「放行」。转发 press 前必须销掉
                // 旧记账：否则 release 被残留标记吃掉 → 幽灵连发（947a3a3 的根因）。
                self.swallowed.forward(code);
                self.forward_key(time, code, true);
            }
            PressAction::Tap => {
                // 组合恰在中途被删空：给应用一对完整 press+release（等效一次
                // 点击，继续删应用字符），表不撤；物理 release 到达才停。
                self.tap_pair(code);
            }
        }
        // chinese() 翻转只可能是 Shift 中英切换（引擎唯一翻转路径，工单第 1 条）：
        // 闪一次对应模式字，下一次按键收掉；常驻角标同时翻面。
        // sync_mode_badge 幂等（set_chinese 没变就不碰 buffer），放这里是为了覆盖
        // 任何一条没走 apply_consumed 的路由。
        let mode_now = self.engine.as_ref().map_or(mode_before, |e| e.chinese());
        self.sync_mode_badge(qh);
        if mode_now != mode_before {
            if let Some(canvas) = self.popup.as_mut() {
                canvas.flash_chip(mode_now, &mut self.renderer, qh);
            }
        }
        self.try_recv_llm(qh);
    }

    /// 向应用发一次完整的按键点击（合成 tick 撞上组合已清空时用）。
    fn tap_pair(&mut self, code: u32) {
        let t = now_ms() as u32;
        self.swallowed.forward(code);
        self.forward_key(t, code, true);
        self.forward_key(t, code, false);
        // 这对点击已闭环：重新记账，物理 release 到达时须被吞、不得再外发。
        self.swallowed.consume(code);
    }

    /// 主循环 poll 超时唤醒时调用：到点的重复在这里合成。
    fn tick_repeats(&mut self, qh: &QueueHandle<Self>) {
        let now = now_ms();
        if let Some(code) = self.repeat.tick(now) {
            self.engine_press(code, now as u32, true, qh);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for AppState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: RegistryEvent,
        _globals: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let RegistryEvent::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_input_method_manager_v2" if version >= 1 => {
                    let mgr =
                        registry.bind::<ZwpInputMethodManagerV2, (), AppState>(name, 1, qh, ());
                    state.input_method_manager = Some(mgr);
                    log(&format!("bound input method manager name={name}"));
                    state.try_bind_im(qh);
                }
                "zwp_virtual_keyboard_manager_v1" if version >= 1 => {
                    let mgr =
                        registry.bind::<ZwpVirtualKeyboardManagerV1, (), AppState>(name, 1, qh, ());
                    state.vk_manager = Some(mgr);
                    log("bound virtual keyboard manager");
                    state.try_bind_vk(qh);
                }
                "wl_compositor" if version >= 1 => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, (), AppState>(name, 1, qh, ()));
                    log(&format!("bound wl_compositor name={name}"));
                    state.ensure_badge(qh);
                }
                "wl_shm" if version >= 1 => {
                    state.shm = Some(registry.bind::<WlShm, (), AppState>(name, 1, qh, ()));
                    log(&format!("bound wl_shm name={name}"));
                    // 角标要三样齐备，global 到达顺序不定：谁最后到谁触发一次
                    // 幂等的建面尝试（ensure_badge 缺项早退）
                    state.ensure_badge(qh);
                }
                // 合成器不支持就没有这一条：角标整条跳过，输入法其余功能不受影响
                "zwlr_layer_shell_v1" if version >= 1 => {
                    let ver = version.min(1);
                    state.layer_shell =
                        Some(registry.bind::<ZwlrLayerShellV1, (), AppState>(name, ver, qh, ()));
                    log(&format!("bound zwlr_layer_shell_v1 name={name} v{ver}"));
                    state.ensure_badge(qh);
                }
                "wl_seat" if version >= 1 => {
                    let ver = version.min(7);
                    let seat = registry.bind::<WlSeat, (), AppState>(name, ver, qh, ());
                    log(&format!("bound wl_seat name={name} v{ver}"));
                    state.seats.push(SeatBind { seat, name: None });
                    state.try_bind_im(qh);
                    state.try_bind_vk(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlRegistry, ()> for AppState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            log(&format!("seat name={name}"));
            if let Some(bind) = state.seats.iter_mut().find(|s| s.seat == *seat) {
                bind.name = Some(name);
            }
            state.try_bind_im(qh);
            state.try_bind_vk(qh);
        }
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for AppState {
    fn event(
        _state: &mut Self,
        _mgr: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _mgr: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _vk: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for AppState {
    fn event(
        state: &mut Self,
        im: &ZwpInputMethodV2,
        event: ZwpInputMethodEvent,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodEvent::Activate => {
                log("input_method ACTIVATE");
                state.input_method = Some(im.clone());
                state.ensure_engine();
                state.ensure_popup(im, qh);
                let grab = im.grab_keyboard(qh, ());
                log("grab_keyboard requested");
                state.grab = Some(grab);
                state.swallowed.clear();
                state.repeat.clear();
                state.shift_gesture.reset();
            }
            ZwpInputMethodEvent::Deactivate => {
                log("input_method DEACTIVATE");
                state.grab = None;
                state.repeat.clear();
                state.shift_gesture.reset();
                // 组合/preedit 一并作废：否则旧应用的拼音和候选带到下一个应用。
                state.clear_composition();
                state.popup_hide(qh);
            }
            // 协议的双缓冲：这三个事件只改 pending，真正的生效在 done。
            ZwpInputMethodEvent::SurroundingText {
                text,
                cursor,
                anchor: _,
            } => {
                state.pending_surrounding = Some((text, cursor as usize));
            }
            ZwpInputMethodEvent::TextChangeCause { cause } => {
                state.pending_cause = wenum_to_u32(cause);
            }
            ZwpInputMethodEvent::ContentType { hint, purpose } => {
                let (hint, purpose) = (wenum_to_u32(hint), wenum_to_u32(purpose));
                // 去重：合成器每次提交都回发 content_type（实测单次打字 1000+ 条），
                // 同值反复 eprintln 阻塞按键路径——只在变化时记日志。
                if state.content_type != Some((hint, purpose)) {
                    state.content_type = Some((hint, purpose));
                    log(&format!("content_type hint={hint} purpose={purpose}"));
                }
                state.pending_content_type = Some((hint, purpose));
            }
            ZwpInputMethodEvent::Done { .. } => {
                // serial 与现有 im_serial 同源，保持既有 commit 流程不变；
                // 提交语义（归一 → 截断 → set_context → 刷新）见 plan_context_commit。
                state.im_serial += 1;
                let action = plan_context_commit(
                    state.pending_surrounding.as_ref(),
                    state.pending_cause,
                    CONTEXT_TAIL_CHARS,
                );
                match state.engine.as_mut() {
                    Some(engine) => match action {
                        ContextCommit::Unchanged => {}
                        ContextCommit::Echo => {
                            log("context: 回声过滤（cause=INPUT_METHOD），不推引擎");
                        }
                        ContextCommit::Apply(tail) => {
                            match &tail {
                                Some(t) => log(&format!("context: 提交上下文尾巴（{t}）")),
                                None => log("context: 无可用上下文（cursor 非法或句首），清空"),
                            }
                            engine.set_context(tail);
                            // 上下文一变立刻重排（组合在途时才有可见效果；
                            // 空组合是 no-op）。不在按键路径里调。
                            engine.refresh_candidates();
                        }
                    },
                    // 词库没开成：上下文丢了就丢了，引擎起来前的状态无意义。
                    None => {}
                }
                // pending 状态是每批一次性消耗的：done 之后清零，
                // 下一批没来 surrounding_text 就是「没变化」而非「清空」。
                state.pending_surrounding = None;
                state.pending_cause = 0;
            }
            ZwpInputMethodEvent::Unavailable => {
                log("input_method UNAVAILABLE — another IM owns the seat");
                state.should_exit = true;
            }
            _ => {}
        }
    }
}
impl Dispatch<WlCompositor, ()> for AppState {
    fn event(
        _state: &mut Self,
        _comp: &WlCompositor,
        _event: <WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WlSurface, ()> for AppState {
    fn event(
        _state: &mut Self,
        _surface: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for AppState {
    fn event(
        _state: &mut Self,
        _shm: &WlShm,
        _event: <WlShm as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for AppState {
    fn event(
        _state: &mut Self,
        _pool: &WlShmPool,
        _event: <WlShmPool as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// 合成器归还候选条的 buffer：对应槽位重新可用，欠的帧立刻补画。
impl Dispatch<WlBuffer, BufferSlot> for AppState {
    fn event(
        state: &mut Self,
        _buffer: &WlBuffer,
        event: <WlBuffer as Proxy>::Event,
        slot: &BufferSlot,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if matches!(event, <WlBuffer as Proxy>::Event::Release) {
            if let Some(canvas) = state.popup.as_mut() {
                canvas.on_release(slot.0 as usize, &mut state.renderer, qh);
            }
        }
    }
}

/// 合成器归还角标的 buffer：与上面按 user data 类型分流，两个表面互不串台。
impl Dispatch<WlBuffer, BadgeSlot> for AppState {
    fn event(
        state: &mut Self,
        _buffer: &WlBuffer,
        event: <WlBuffer as Proxy>::Event,
        slot: &BadgeSlot,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if matches!(event, <WlBuffer as Proxy>::Event::Release) {
            if let Some(badge) = state.badge.as_mut() {
                badge.on_release(slot.0 as usize, &mut state.renderer, qh);
            }
        }
    }
}

/// frame 回调（候选条）：上一帧已被合成器显示，可以安全画下一帧。
impl Dispatch<WlCallback, ()> for AppState {
    fn event(
        state: &mut Self,
        _cb: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let Some(canvas) = state.popup.as_mut() {
            canvas.on_frame(&mut state.renderer, qh);
        }
    }
}

/// frame 回调（角标）：与上面按 user data 类型分流——同一 Proxy 可以有多份
/// `Dispatch`，Rust 依 `Data` 选派发。两个表面都无脑重画是浪费（present 不看
/// dirty，每次都真画一帧）。
impl Dispatch<WlCallback, BadgeFrame> for AppState {
    fn event(
        state: &mut Self,
        _cb: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &BadgeFrame,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let Some(badge) = state.badge.as_mut() {
            badge.on_frame(&mut state.renderer, qh);
        }
    }
}

/// 空 input region：只为满足点击穿透，`create_region` 之后立刻 drop，本对象
/// 不会收到任何事件。
impl Dispatch<WlRegion, ()> for AppState {
    fn event(
        _state: &mut Self,
        _region: &WlRegion,
        _event: <WlRegion as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// layer shell 本体：只用来 get_layer_surface，自身不发任何事件。
impl Dispatch<ZwlrLayerShellV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _shell: &ZwlrLayerShellV1,
        _event: <ZwlrLayerShellV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

/// layer surface 事件：configure 是「现在可以贴第一帧」的信号（ack 由 ModeBadge
/// 内部完成，必须先于下一个 commit）；closed 是合成器收走表面，之后不再重建。
impl Dispatch<ZwlrLayerSurfaceV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _surface: &ZwlrLayerSurfaceV1,
        event: ZwlrLayerSurfaceEvent,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwlrLayerSurfaceEvent::Configure {
                serial,
                width,
                height,
            } => {
                log(&format!("badge configure {width}x{height} serial={serial}"));
                if let Some(badge) = state.badge.as_mut() {
                    badge.on_configure(serial, width, height);
                    // on_configure 置了 dirty：首帧必须画出来，否则表面映射了也是空白
                    badge.present(&mut state.renderer, qh);
                }
            }
            ZwlrLayerSurfaceEvent::Closed => {
                log("badge layer surface 被合成器关闭，不再重建");
                state.badge = None;
            }
            _ => {}
        }
    }
}

/// 候选窗只画进这个 surface，摆位全权交给合成器：rect 不再参与布局，只留日志 —
impl Dispatch<ZwpInputPopupSurfaceV2, ()> for AppState {
    fn event(
        state: &mut Self,
        _popup: &ZwpInputPopupSurfaceV2,
        event: ZwpInputPopupSurfaceEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ZwpInputPopupSurfaceEvent::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            // 摆位归 popup role 与合成器（surface 只 attach(0,0)）；rect 是
            // 「我们真的被 map 并被摆位了」的诊断证据，不参与布局。
            let rect = (x, y, width, height);
            if state.cursor_rect != Some(rect) {
                log(&format!("caret rect {x},{y} {width}x{height}"));
            }
            state.cursor_rect = Some(rect);
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for AppState {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: ZwpInputMethodKeyboardGrabEvent,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodKeyboardGrabEvent::Keymap { format, fd, size } => {
                log(&format!("grab keymap size={size}"));
                let fmt = match format {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(v) => v,
                };
                // 自解码：grab 建立时合成器就发一次 keymap，之后变化时再发；
                // 这里把它编译成 xkb Keymap，失败/格式不支持则沿用上一份（比没有强，也比错表强）。
                if fmt == KEYMAP_FORMAT_XKB_V1 {
                    // fd 偏移可能不在 0（dup 共享偏移，合成器/wayland 库可能已读过），
                    // 读前必须复位，否则 read_to_string 读到空 → XKB-822 解析失败。
                    let loaded = dup_fd(&fd)
                        .map(File::from)
                        .and_then(|mut f| {
                            let _ = unsafe { libc::lseek(f.as_raw_fd(), 0, libc::SEEK_SET) };
                            let mut text = String::new();
                            f.read_to_string(&mut text).ok().map(|_| text)
                        })
                        .is_some_and(|text| state.keyboard.set_keymap(&text));
                    log(if loaded {
                        "xkb keymap 已编译"
                    } else {
                        "xkb keymap 读取/编译失败"
                    });
                } else {
                    log(&format!("不支持的 keymap format {fmt}，xkb 解码未就绪"));
                }
                if let Some(vk) = &state.vk {
                    if let Some(dup) = dup_fd(&fd) {
                        vk.keymap(fmt, dup.as_fd(), size);
                        state.vk_keymap_ready = true;
                        log("vk keymap set");
                    } else {
                        log("dup keymap fd failed");
                    }
                } else {
                    log("vk missing when keymap arrived");
                }
            }
            ZwpInputMethodKeyboardGrabEvent::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                // 自己的 xkb state 必须跟着合成器走：shift 层字符全靠它
                state
                    .keyboard
                    .update_mods(mods_depressed, mods_latched, mods_locked, group);
                // 真机诊断：mangowm 是否真发 Control/Alt 位（决定旗标推导是否可信）。
                log(&format!(
                    "Modifiers depressed=0x{mods_depressed:x} ctrl={} alt={}",
                    state.keyboard.ctrl(),
                    state.keyboard.alt()
                ));
                if let Some(vk) = &state.vk {
                    if state.vk_keymap_ready {
                        vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                    }
                }
            }
            ZwpInputMethodKeyboardGrabEvent::Key {
                serial: _,
                time,
                key,
                state: key_state,
            } => {
                let pressed = matches!(key_state, WEnum::Value(KeyState::Pressed));
                let released = matches!(key_state, WEnum::Value(KeyState::Released));

                // 键码嗅探只是时序兜底，旗标归属 keyboard.rs（Modifiers 推导权威）。
                state.keyboard.note_key(key, pressed);
                let is_shift = matches!(key, 42 | 54);

                if released {
                    if is_shift && state.shift_gesture.on_shift_release() == ShiftRelease::Toggle {
                        // 手势裁决：只有点击（其间没打过别的键）才把这一次 Shift 补交
                        // 给引擎切中英；按住打过键 → 模式不动（rime ascii_composer）。
                        state.engine_press(key, time, false, qh);
                    }
                    // 撤表只认自己（Shift/Ctrl 的 release 停不了 F 的重复），随后按
                    // press 记账决定 release 吞放。route_release 见 route.rs。
                    if route_release(key, &mut state.swallowed, &mut state.repeat) {
                        state.forward_key(time, key, false);
                    }
                    return;
                }
                if !pressed {
                    return;
                }

                // 模式闪现生命周期：任何一次按下（消费或放行都算）先收掉上一次闪现。
                // release 不收：切换裁决在 Shift 松手处，抬起若也清，闪现活不过一个手势。
                if let Some(canvas) = state.popup.as_mut() {
                    canvas.clear_chip(&mut state.renderer, qh);
                }

                if is_shift {
                    // 引擎就绪且无组合 → press 什么都不做，裁决推迟到 release；
                    // 有组合/引擎缺失 → 老路径原样（press 立即喂引擎，可触发顶串切英文）。
                    if state
                        .engine
                        .as_ref()
                        .is_some_and(|e| e.preedit().is_empty())
                    {
                        state.swallowed.consume(key);
                        state.shift_gesture.on_shift_press(now_ms());
                    } else {
                        state.engine_press(key, time, false, qh);
                    }
                    return;
                }
                // Shift 按住期间：字母原样透传（大写英文形态，应用自己 xkb 解码）；
                // 标点键走引擎——rime 的 ascii_composer 只影响字母，punctuator
                // 不受 shift 位影响（Shift hold 打标点出中文标点）。裁决契约见
                // route::shift_holds_passthrough；方向/Home/End 照旧透传。
                if shift_holds_passthrough(
                    state.shift_gesture.on_key_press(),
                    state.keyboard.key_char(key),
                    state.keyboard.alt(),
                ) {
                    state.forward_key(time, key, true);
                    return;
                }
                // xkb 取代手写码表：功能键（含空格/Enter/Esc）utf32 为空或控制字符
                // → None，由引擎按 code 处理。引擎路径（消费/上屏/放行 + 重复起表）
                // 收敛在 engine_press 一处。
                state.engine_press(key, time, false, qh);
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut target_seat = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--seat" && i + 1 < args.len() {
            target_seat = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    if let Some(s) = &target_seat {
        log(&format!("目标 seat: {s}"));
    } else {
        log("使用默认 seat");
    }

    // 冷路径预热前移到进程启动：候选窗首次渲染含 CJK 扩展区字形的页时，
    // 缺字 fallback 全字体线扫 + 字体解析实测 134ms 帧延迟（~110ms 纯 CPU），
    // 绝不能落在首个 ACTIVATE 的按键同步路径上。Renderer 不持有任何 wayland
    // 对象（像素缓冲由调用方提供），所以在连 wayland 之前同步构造即可：
    // 守护进程启动期用户不可见，不为此引入线程/异步。
    let renderer = Renderer::new();

    let conn = Connection::connect_to_env()?;

    let (globals, mut event_queue) = registry_queue_init::<AppState>(&conn)?;
    let qh: QueueHandle<AppState> = event_queue.handle();
    let mut app = AppState::new(target_seat, renderer);
    app.conn = Some(conn);

    globals.contents().with_list(|list| {
        for global in list {
            match global.interface.as_str() {
                "zwp_input_method_manager_v2" if global.version >= 1 => {
                    log(&format!(
                        "Found IM manager: {} v{}",
                        global.name, global.version
                    ));
                    let mgr = globals
                        .registry()
                        .bind::<ZwpInputMethodManagerV2, (), AppState>(global.name, 1, &qh, ());
                    app.input_method_manager = Some(mgr);
                }
                "zwp_virtual_keyboard_manager_v1" if global.version >= 1 => {
                    log("Found virtual keyboard manager");
                    let mgr = globals
                        .registry()
                        .bind::<ZwpVirtualKeyboardManagerV1, (), AppState>(global.name, 1, &qh, ());
                    app.vk_manager = Some(mgr);
                }
                "wl_seat" if global.version >= 1 => {
                    let ver = global.version.min(7);
                    log(&format!("Found seat: {} v{ver}", global.name));
                    let seat =
                        globals
                            .registry()
                            .bind::<WlSeat, (), AppState>(global.name, ver, &qh, ());
                    app.seats.push(SeatBind { seat, name: None });
                }
                "wl_compositor" if global.version >= 1 => {
                    log("Found wl_compositor");
                    app.compositor = Some(globals.registry().bind::<WlCompositor, (), AppState>(
                        global.name,
                        1,
                        &qh,
                        (),
                    ));
                }
                "wl_shm" if global.version >= 1 => {
                    log("Found wl_shm");
                    app.shm = Some(globals.registry().bind::<WlShm, (), AppState>(
                        global.name,
                        1,
                        &qh,
                        (),
                    ));
                }
                "zwlr_layer_shell_v1" if global.version >= 1 => {
                    let ver = global.version.min(1);
                    log(&format!("Found zwlr_layer_shell_v1 v{ver}"));
                    app.layer_shell =
                        Some(globals.registry().bind::<ZwlrLayerShellV1, (), AppState>(
                            global.name,
                            ver,
                            &qh,
                            (),
                        ));
                }
                _ => {}
            }
        }
    });
    app.try_bind_im(&qh);
    app.try_bind_vk(&qh);
    // 角标要 layer-shell + compositor + shm 三者齐备，而上面遍历顺序不定，
    // 所以统一在遍历结束后建一次（ensure_badge 内部自带幂等与缺项早退）
    app.ensure_badge(&qh);
    log("event loop");

    // 手写 poll 循环取代 blocking_dispatch：合成器不给 IM grab 投 repeat，长按节奏
    // 要靠本循环在「无 wayland 事件」时也被定时器叫醒；poll 超时本身充当计时器，
    // 不引 calloop/timerfd 任何新依赖（libc 已在依赖树）。
    let backend = app.conn.as_ref().expect("conn set above").backend();
    let display_fd = backend.poll_fd().as_raw_fd();
    while !app.should_exit {
        event_queue.flush()?;
        let timeout_ms: i32 = match app.repeat.next_due() {
            Some(due) => due.saturating_sub(now_ms()).min(i32::MAX as u64) as i32,
            None => -1,
        };
        let mut pfd = [libc::pollfd {
            fd: display_fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: pfd 是有效的单个 pollfd，nfds=1。
        let ret = unsafe { libc::poll(pfd.as_mut_ptr(), pfd.len() as libc::nfds_t, timeout_ms) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err.into());
        }
        let woke = pfd[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0;
        if woke {
            // 只在 socket 确实可读时进 prepare_read().read()（它会阻塞等数据）。
            if let Some(guard) = event_queue.prepare_read() {
                guard.read()?;
            }
        } else {
            app.tick_repeats(&qh);
        }
        event_queue.dispatch_pending(&mut app)?;
    }
    log("exit");
    Ok(())
}
