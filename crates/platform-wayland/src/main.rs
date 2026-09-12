//! input-method-v2 平台壳：绑 seat → get_input_method → grab → engine。
//! 未消费的键经 zwp_virtual_keyboard_v1 原样送回，避免 grab 吞掉回车/快捷键。

use std::collections::HashSet;
use std::env;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc::{channel, Receiver, Sender};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::llm::{Debouncer, LlmClient};
use kime_core::{Engine, Key, Outcome};
use platform_wayland::tray::TrayIconManager;
use platform_wayland::{Layout, Renderer};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_keyboard::KeyState,
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

fn log(msg: &str) {
    eprintln!("[kime-ime] {}", msg);
}

fn dup_fd(fd: impl AsFd) -> Option<OwnedFd> {
    let raw = unsafe { libc::dup(fd.as_fd().as_raw_fd()) };
    if raw < 0 {
        None
    } else {
        Some(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

/// US QWERTY evdev KEY_* → 字符。功能键 ch=None，靠 code 走 engine。
fn evdev_char(code: u32) -> Option<char> {
    match code {
        16 => Some('q'),
        17 => Some('w'),
        18 => Some('e'),
        19 => Some('r'),
        20 => Some('t'),
        21 => Some('y'),
        22 => Some('u'),
        23 => Some('i'),
        24 => Some('o'),
        25 => Some('p'),
        30 => Some('a'),
        31 => Some('s'),
        32 => Some('d'),
        33 => Some('f'),
        34 => Some('g'),
        35 => Some('h'),
        36 => Some('j'),
        37 => Some('k'),
        38 => Some('l'),
        44 => Some('z'),
        45 => Some('x'),
        46 => Some('c'),
        47 => Some('v'),
        48 => Some('b'),
        49 => Some('n'),
        50 => Some('m'),
        2 => Some('1'),
        3 => Some('2'),
        4 => Some('3'),
        5 => Some('4'),
        6 => Some('5'),
        7 => Some('6'),
        8 => Some('7'),
        9 => Some('8'),
        10 => Some('9'),
        11 => Some('0'),
        12 => Some('-'),
        13 => Some('='),
        26 => Some('['),
        27 => Some(']'),
        52 => Some('.'),
        _ => None,
    }
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

    fn request(&self, syllables: Vec<String>) {
        let client = self.client.clone();
        let debouncer = self.debouncer.clone();
        let sender = self.result_sender.clone();
        self.runtime.spawn(async move {
            if debouncer.should_fire().await {
                match client.request_async(syllables).await {
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

/// wl_buffer user data：release 回来后该槽位重新可用。
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
    renderer: Renderer,
    buffers: [Option<ShmBuffer>; 2],
    free: [bool; 2],
    frame: Option<WlCallback>,
    /// 新内容因无空闲 buffer 未能上屏：release/frame 到达即补画
    dirty: bool,
    /// 待显示内容：当前页候选 + 页内高亮下标
    content: Vec<String>,
    highlight: usize,
    /// 引擎中英模式：英文 + 无候选 = 画 `英` 提示小窗；中文 + 无候选 = 隐藏
    chinese: bool,
}

impl PopupCanvas {
    fn new(
        im: &ZwpInputMethodV2,
        comp: &WlCompositor,
        shm: &WlShm,
        qh: &QueueHandle<AppState>,
    ) -> Self {
        let surface = comp.create_surface(qh, ());
        // 必须用当前这个 zwp_input_method_v2 实例建 popup（历史上绑错过 IME 对象）
        let _role = im.get_input_popup_surface(&surface, qh, ());
        let mut canvas = Self {
            surface,
            _role,
            shm: shm.clone(),
            renderer: Renderer::new(),
            buffers: [None, None],
            free: [true, true],
            frame: None,
            dirty: false,
            content: Vec::new(),
            highlight: 0,
            chinese: true,
        };
        canvas.present(qh);
        canvas
    }

    fn set_content(
        &mut self,
        candidates: Vec<String>,
        highlight: usize,
        chinese: bool,
        qh: &QueueHandle<AppState>,
    ) {
        if self.content == candidates && self.highlight == highlight && self.chinese == chinese {
            return;
        }
        self.content = candidates;
        self.highlight = highlight;
        self.chinese = chinese;
        self.present(qh);
    }

    /// 强制隐藏（deactivate 用）：即便引擎在英文模式也不留提示窗。
    /// 中文 + 空候选正是 layout() 的隐藏帧条件。
    fn hide(&mut self, qh: &QueueHandle<AppState>) {
        self.set_content(Vec::new(), 0, true, qh);
    }

    fn on_release(&mut self, slot: usize, qh: &QueueHandle<AppState>) {
        if slot < 2 {
            self.free[slot] = true;
        }
        if self.dirty {
            self.present(qh);
        }
    }

    fn on_frame(&mut self, qh: &QueueHandle<AppState>) {
        self.frame = None;
        if self.dirty {
            self.present(qh);
        }
    }

    /// 布局 → 找空闲槽位 → 画 → attach + commit + frame 请求。
    fn present(&mut self, qh: &QueueHandle<AppState>) {
        self.dirty = false;
        let layout = self.renderer.layout(&self.content, self.chinese);
        let Some(slot) = self.pick_slot(&layout) else {
            self.dirty = true;
            return;
        };
        let reuse = self.buffers[slot].as_ref().is_some_and(|b| {
            b.w == layout.width && b.h == layout.height && b.len == layout.pixel_len()
        });
        if !reuse {
            // 槽位里的旧 buffer 必已 release（free 才会走到这），换掉安全
            match Self::create_buffer(&self.shm, &layout, slot, qh) {
                Ok(b) => self.buffers[slot] = Some(b),
                Err(e) => {
                    log(&format!("popup buffer 分配失败: {e}"));
                    return;
                }
            }
        }
        {
            let b = self.buffers[slot].as_ref().unwrap();
            let pixels = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
            self.renderer.paint(&layout, self.highlight, pixels);
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
        slot: usize,
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
            BufferSlot(slot as u8),
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
    ctrl: bool,
    alt: bool,
    shift: bool,
    /// grab 吃掉 press 的键，release 也不转发，避免半截按键
    swallowed: HashSet<u32>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    /// 候选窗：画候选词 + 中英模式提示；text_input_rectangle 只留日志
    popup: Option<PopupCanvas>,
}

impl AppState {
    fn new(target_seat: Option<String>) -> Self {
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
            ctrl: false,
            alt: false,
            shift: false,
            swallowed: HashSet::new(),
            compositor: None,
            shm: None,
            popup: None,
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
                let endpoint = config.ai_endpoint.clone().unwrap_or_default();
                if !endpoint.is_empty() {
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
        self.popup = Some(PopupCanvas::new(im, &comp, &shm, qh));
        log("input popup 候选窗已建（已提交 1×1 透明首帧）");
    }

    /// 引擎当前页候选 + 页内高亮 + 中英模式 → 进程内渲染层。
    /// 只画当前页：highlight() 是页首全局下标，页内 = highlight()-start；
    /// 数字键 1..=9/0 与标号天然对齐。不画 preedit（应用自己内联显示）。
    /// 英文模式候选恒为空 → 渲染层退化为只画 `英` 的提示小窗。
    fn popup_show(&mut self, qh: &QueueHandle<AppState>) {
        let (page, hl, chinese) = match &self.engine {
            Some(engine) => {
                let (start, ps) = (engine.highlight(), engine.page().1);
                let all = engine.candidates();
                let page = all[start.min(all.len())..(start + ps).min(all.len())]
                    .iter()
                    .map(|c| c.text.clone())
                    .collect();
                (
                    page,
                    engine.highlight().saturating_sub(start),
                    engine.chinese(),
                )
            }
            None => (Vec::new(), 0, true),
        };
        if let Some(canvas) = self.popup.as_mut() {
            canvas.set_content(page, hl, chinese, qh);
        }
    }

    /// 隐藏 = 1×1 全透明帧，绝不 destroy surface（可见性归 IM active 状态管）。
    /// 只在 deactivate 用；键流上的清空走 popup_show（英文模式要显示提示窗）。
    fn popup_hide(&mut self, qh: &QueueHandle<AppState>) {
        if let Some(canvas) = self.popup.as_mut() {
            canvas.hide(qh);
        }
    }

    fn apply_consumed(&mut self, qh: &QueueHandle<Self>) {
        let syllables = {
            let Some(engine) = self.engine.as_ref() else {
                return;
            };
            self.tray.set_chinese(engine.chinese());
            let pe = engine.preedit().to_string();
            log(&format!("consumed preedit={pe}"));
            if let Some(im) = &self.input_method {
                let cursor = pe.len() as i32;
                im.set_preedit_string(pe.clone(), 0, cursor);
                im.commit(self.im_serial);
            }
            pe.split('\'').map(String::from).collect::<Vec<_>>()
        };
        self.popup_show(qh);
        if let Some(worker) = &self.llm_worker {
            worker.request(syllables);
        }
    }

    fn apply_commit(&mut self, text: String, qh: &QueueHandle<Self>) {
        log(&format!("commit {text}"));
        if let Some(im) = &self.input_method {
            im.commit_string(text);
            im.set_preedit_string(String::new(), 0, 0);
            im.commit(self.im_serial);
        }
        // 走 popup_show 而非 hide：Shift 上屏原串会同时切到英文模式，
        // 此时要显示 `英` 提示窗；中文模式下候选已被引擎清空 → 自然回到隐藏帧。
        self.popup_show(qh);
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
                }
                "wl_shm" if version >= 1 => {
                    state.shm = Some(registry.bind::<WlShm, (), AppState>(name, 1, qh, ()));
                    log(&format!("bound wl_shm name={name}"));
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
                // 英文模式下 activate 也要立刻见 `英` 提示，不等第一次按键
                state.popup_show(qh);
                let grab = im.grab_keyboard(qh, ());
                log("grab_keyboard requested");
                state.grab = Some(grab);
                state.swallowed.clear();
            }
            ZwpInputMethodEvent::Deactivate => {
                log("input_method DEACTIVATE");
                state.grab = None;
                state.swallowed.clear();
                state.popup_hide(qh);
            }
            ZwpInputMethodEvent::Done { .. } => {
                state.im_serial += 1;
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

/// 合成器归还 buffer：对应槽位重新可用，欠的帧立刻补画。
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
                canvas.on_release(slot.0 as usize, qh);
            }
        }
    }
}

/// frame 回调：上一帧已被合成器显示，可以安全画下一帧。
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
            canvas.on_frame(qh);
        }
    }
}

/// 候选窗只画进这个 surface，摆位全权交给合成器：rect 不再参与布局，只留日志 —
/// 它是「我们真的被 map 并被摆位了」的唯一真机验收证据。
impl Dispatch<ZwpInputPopupSurfaceV2, ()> for AppState {
    fn event(
        _state: &mut Self,
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
            log(&format!("caret rect {x},{y} {width}x{height}"));
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
                if let Some(vk) = &state.vk {
                    if let Some(dup) = dup_fd(&fd) {
                        let fmt = match format {
                            WEnum::Value(v) => v as u32,
                            WEnum::Unknown(v) => v,
                        };
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

                match key {
                    29 | 97 => state.ctrl = pressed,
                    56 | 100 => state.alt = pressed,
                    42 | 54 => state.shift = pressed,
                    _ => {}
                }

                if released {
                    if state.swallowed.remove(&key) {
                        return;
                    }
                    state.forward_key(time, key, false);
                    return;
                }
                if !pressed {
                    return;
                }
                let is_shift = matches!(key, 42 | 54);
                if state.alt || state.ctrl {
                    state.forward_key(time, key, true);
                    return;
                }
                let ch = if matches!(key, 1 | 14 | 28 | 42 | 54 | 57) {
                    None
                } else {
                    evdev_char(key)
                };

                let Some(engine) = state.engine.as_mut() else {
                    state.forward_key(time, key, true);
                    return;
                };
                let key_struct = Key {
                    ch,
                    code: key,
                    shift: is_shift,
                    ctrl: state.ctrl,
                    alt: state.alt,
                };
                match engine.key(key_struct) {
                    Outcome::Consumed => {
                        state.swallowed.insert(key);
                        state.apply_consumed(qh);
                    }
                    Outcome::Commit(text) => {
                        state.swallowed.insert(key);
                        state.apply_commit(text, qh);
                    }
                    Outcome::Ignored => {
                        state.forward_key(time, key, true);
                    }
                }
                state.try_recv_llm(qh);
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

    let conn = Connection::connect_to_env()?;

    let (globals, mut event_queue) = registry_queue_init::<AppState>(&conn)?;
    let qh: QueueHandle<AppState> = event_queue.handle();
    let mut app = AppState::new(target_seat);
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
                _ => {}
            }
        }
    });
    app.try_bind_im(&qh);
    app.try_bind_vk(&qh);
    log("event loop");

    while !app.should_exit {
        event_queue.blocking_dispatch(&mut app)?;
    }
    log("exit");
    Ok(())
}
