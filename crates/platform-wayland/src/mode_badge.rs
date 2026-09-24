//! 常驻输入模式徽标（无合成串时的非合成场景）：一个 zwlr layer-shell 覆盖层
//! 表面，固定贴在屏幕右上角，常显「中」/「英」。
//!
//! 吃鼠标点击、抢键盘焦点都是硬性禁止：徽标只是被动显示当前模式，一旦输入
//! 穿透失效就会挡住用户本来要点的东西。所以 keyboard_interactivity=None 与
//! 空 input region 两者缺一不可。
//!
//! buffer 生命周期（memfd + mmap + wl_shm pool、双槽记账、frame 回调）刻意
//! 与 main.rs 的 PopupCanvas 保持同构，便于对照排障。

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_region::WlRegion,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

use crate::render::{Layout, Renderer};

/// 徽标开关环境变量：未设置/空串=开；仅 `0`/`false`/`off`/`no`=关；其余按开处理。
pub const ENV: &str = "KIME_MODE_BADGE";

/// 显式关闭的字面量（trim + 不分大小写）
fn is_falsy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// 纯函数：None（未设置）= 开；否则非 falsy 即开。
pub fn badge_enabled_from(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(v) => !is_falsy(v),
    }
}

/// 读进程 env。无法识别的取值按「开」处理并 eprintln 告警一次。
pub fn badge_enabled() -> bool {
    match env::var(ENV) {
        Ok(v) => {
            if !v.trim().is_empty() && !is_recognized(&v) {
                warn_once(&format!("{ENV} 取值「{v}」无法识别，模式徽标按开启处理"));
            }
            badge_enabled_from(Some(&v))
        }
        Err(env::VarError::NotPresent) => true,
        Err(env::VarError::NotUnicode(_)) => {
            warn_once(&format!("{ENV} 取值非 UTF-8，模式徽标按开启处理"));
            true
        }
    }
}

/// 显式开启的字面量；与 `is_falsy` 的关闭集合并起来即全部已知拼写。
/// 只服务于告警判定：解析本身一律走 `badge_enabled_from`。
fn is_recognized(value: &str) -> bool {
    is_falsy(value)
        || matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
}

fn warn_once(msg: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("[kime] 警告：{msg}");
    }
}

/// wl_buffer user data：合成器归还 buffer 后据此找回槽位。
/// 与 main.rs 候选窗的 BufferSlot 刻意分开：两个模块各有各的 Dispatch 实现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadgeSlot(pub u8);

/// wl_callback user data：把 frame 事件标记为「角标的」，否则分不清是候选条
/// 还是角标那一块。候选条用 `()`，Rust 允许同一 Proxy 按 user data 类型并存
/// 多个 `Dispatch` 实现，于是两边各自只收自己的回调。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadgeFrame;

/// 一块 shm 画布：memfd 与其 mmap 必须同源（渲染写与合成器读必须落在同一段
/// 内存）；Drop 时 munmap+close，WlBuffer 代理丢弃即 destroy。
struct BadgeShmBuffer {
    buffer: WlBuffer,
    fd: i32,
    ptr: *mut u8,
    len: usize,
    w: u32,
    h: u32,
}

impl Drop for BadgeShmBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
            libc::close(self.fd);
        }
    }
}

/// 常驻模式徽标：layer surface + 简单双缓冲。
///
/// 表面只建一次、绝不销毁；常显与否由上层在启用时创建、停用时丢弃。
///
/// 不持有 `Renderer`：徽标与候选条共用 `AppState` 里的同一个，绘制时由调用方
/// 传 `&mut Renderer` 进来（见 `present`）。徽标只在模式翻转时重画，而翻转发生
/// 在按键路径上——这里若另建一个渲染器，第一次 Shift 就得再付一次 cosmic-text
/// 缺字 fallback 的全字体线扫（实测 ~110ms 纯 CPU），等于把上一轮修掉的 ng 首卡
/// 在徽标上原样复现一遍。共用则单 FontSystem、单次预热、按键路径零冷成本。
pub struct ModeBadge {
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    shm: WlShm,
    buffers: [Option<BadgeShmBuffer>; 2],
    free: [bool; 2],
    frame: Option<WlCallback>,
    /// 新内容因无空闲 buffer 未能上屏：release/frame 到达即补画
    dirty: bool,
    chinese: bool,
    /// 合成器是否已发过 configure：未 configure 前禁止 attach buffer
    configured: bool,
    /// 合成器指派尺寸（configure 事件的 width/height），只记录备查
    width: u32,
    height: u32,
}

impl ModeBadge {
    /// 建表面并完成 layer-shell 的首次无 buffer commit。
    ///
    /// 协议要求：get_layer_surface 之后必须先做一次不带 buffer 的 commit，
    /// 合成器才会回 configure 事件，此后才允许 attach buffer 并再次 commit。
    /// 顺序反了表面永远不出现，且这类问题 CI 编不出来、只有真机能看出来。
    ///
    /// `layer_shell` 由调用方按合成器通告的版本绑定（不超过其最高版本）；
    /// output 传 None，由合成器自选输出。
    pub fn new<Data>(
        layer_shell: &ZwlrLayerShellV1,
        comp: &WlCompositor,
        shm: &WlShm,
        qh: &QueueHandle<Data>,
    ) -> Self
    where
        Data: Dispatch<WlSurface, ()>
            + Dispatch<WlRegion, ()>
            + Dispatch<ZwlrLayerSurfaceV1, ()>
            + 'static,
    {
        let surface = comp.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "kime".to_string(),
            qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Right);
        layer_surface.set_margin(8, 8, 8, 8);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1);
        // 协议原文：layer surface 默认照收 pointer/touch/tablet，要点击穿透
        // 必须显式把 input region 置空；与 keyboard_interactivity=None 各管
        // 一半输入通道，少任何一个徽标都会吃掉点击。
        let region = comp.create_region(qh, ());
        surface.set_input_region(Some(&region));
        drop(region);
        // 首次 commit 刻意不带 buffer：只用来换 configure 事件。
        surface.commit();
        Self {
            surface,
            layer_surface,
            shm: shm.clone(),
            buffers: [None, None],
            free: [true, true],
            frame: None,
            dirty: false,
            chinese: true,
            configured: false,
            width: 0,
            height: 0,
        }
    }

    /// 合成器是否已发过 configure：false 时 `present` 不会碰 buffer。
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// 合成器在 configure 里指派的尺寸，仅供诊断/排障。
    pub fn configured_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 是否欠一次重画（无空闲槽位时置位，等 release/frame 补上）。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 收 configure：先 ack(serial)，再记尺寸并标脏等 `present` 上屏。
    ///
    /// ack 收在类型内部而不是留给调用方：协议要求它必须先于下一个 commit，而
    /// 下一个 commit 全仓库只有 `present` 会发。拆到外面等于要求接线的人记住
    /// 一条顺序约束，忘掉的代价是表面永远不显示——而这类问题 CI 编不出来、
    /// 单测也测不到，只有真机能看出来。
    pub fn on_configure(&mut self, serial: u32, width: u32, height: u32) {
        self.layer_surface.ack_configure(serial);
        self.width = width;
        self.height = height;
        self.configured = true;
        self.dirty = true;
    }

    /// 切换显示模式，返回是否真的变了——调用方仅在 true 时重画。
    pub fn set_chinese(&mut self, chinese: bool) -> bool {
        let changed = self.chinese != chinese;
        self.chinese = chinese;
        changed
    }

    /// 布局 → 找空闲槽位 → 画 → attach + commit + frame 请求。
    ///
    /// `renderer` 是候选条那个已经 warmup 过的渲染器（见类型注释），不新建。
    ///
    /// 前置条件：必须已收到 configure。方法自身会再挡一道（未 configure 直接
    /// 返回），确保不会把 buffer 贴到尚未收到 configure 的 layer surface 上。
    pub fn present<Data>(&mut self, renderer: &mut Renderer, qh: &QueueHandle<Data>)
    where
        Data: Dispatch<WlShmPool, ()>
            + Dispatch<WlBuffer, BadgeSlot>
            + Dispatch<WlCallback, BadgeFrame>
            + 'static,
    {
        if !self.configured {
            return;
        }
        self.dirty = false;
        let layout = renderer.chip_layout(self.chinese);
        let Some(slot) = self.pick_slot(&layout) else {
            log("mode-badge: 两槽位都被合成器占用，present 挂起（等 release/frame）");
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
                    log(&format!("mode-badge buffer 分配失败: {e}"));
                    return;
                }
            }
        }
        // 走到这槽位必有 buffer 且尺寸与 layout 一致（复用判定或刚按 layout
        // 新建）；仍用 let-else 兜住，不拿运行期可失败的路径去 unwrap。
        let Some(b) = self.buffers[slot].as_ref() else {
            log("mode-badge: 槽位无 buffer，跳过本次 present");
            return;
        };
        // 不变式：free[slot] 为真 ⇒ 合成器已归还此 buffer，不会再读这段内存
        let pixels = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
        renderer.paint(&layout, 0, pixels);
        self.surface.attach(Some(&b.buffer), 0, 0);
        self.surface
            .damage(0, 0, layout.width as i32, layout.height as i32);
        self.surface.commit();
        self.free[slot] = false;
        self.frame = Some(self.surface.frame(qh, BadgeFrame));
    }

    /// 合成器归还 buffer：对应槽位重新可用，欠的帧立刻补画。
    pub fn on_release<Data>(&mut self, slot: usize, renderer: &mut Renderer, qh: &QueueHandle<Data>)
    where
        Data: Dispatch<WlShmPool, ()>
            + Dispatch<WlBuffer, BadgeSlot>
            + Dispatch<WlCallback, BadgeFrame>
            + 'static,
    {
        log(&format!("mode-badge: buffer release slot={slot}"));
        if slot < 2 {
            self.free[slot] = true;
        }
        if self.dirty {
            self.present(renderer, qh);
        }
    }

    /// frame 回调：上一帧已被合成器显示，可以安全画下一帧。
    pub fn on_frame<Data>(&mut self, renderer: &mut Renderer, qh: &QueueHandle<Data>)
    where
        Data: Dispatch<WlShmPool, ()>
            + Dispatch<WlBuffer, BadgeSlot>
            + Dispatch<WlCallback, BadgeFrame>
            + 'static,
    {
        log("mode-badge: frame 回调到达");
        self.frame = None;
        if self.dirty {
            self.present(renderer, qh);
        }
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
    fn create_buffer<Data>(
        shm: &WlShm,
        layout: &Layout,
        slot: usize,
        qh: &QueueHandle<Data>,
    ) -> Result<BadgeShmBuffer, &'static str>
    where
        Data: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, BadgeSlot> + 'static,
    {
        let (w, h) = (layout.width, layout.height);
        if w == 0 || h == 0 || w > 8192 || h > 1024 {
            return Err("mode-badge 尺寸非法");
        }
        let len = (w as usize) * (h as usize) * 4;
        let fd = unsafe { libc::memfd_create(c"kime-badge".as_ptr(), libc::MFD_CLOEXEC) };
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
            BadgeSlot(slot as u8),
        );
        pool.destroy();
        Ok(BadgeShmBuffer {
            buffer,
            fd,
            ptr: ptr as *mut u8,
            len,
            w,
            h,
        })
    }
}

fn log(msg: &str) {
    eprintln!("[kime-ime] {msg}");
}
