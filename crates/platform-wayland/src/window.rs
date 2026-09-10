//! M4: layer-shell 候选窗（真实现：wl_shm buffer + configure 握手 + 像素上屏）
//!
//! 独立第二连接：与主 daemon 的 IME 连接互不阻塞；show()/hide() 内部自轮询。
//! 键盘交互 None——键盘已被 input-method grab 持有。

use std::os::unix::io::AsRawFd;

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_shm, wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::ZwpInputMethodV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::render::{Candidate, Renderer};

const WIDTH: u32 = 420;
const HEIGHT: u32 = 380;

enum LayerSurface {
    Layer(zwlr_layer_surface_v1::ZwlrLayerSurfaceV1),
    Popup(ZwpInputPopupSurfaceV2),
}

impl LayerSurface {
    fn destroy(&self) {
        match self {
            LayerSurface::Layer(ls) => ls.destroy(),
            LayerSurface::Popup(ls) => ls.destroy(),
        }
    }
}

struct WinState {
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    shm: Option<wl_shm::WlShm>,
    compositor: Option<WlCompositor>,
    configure_serial: Option<u32>,
    configured: bool,
    closed: bool,
}

pub struct CandidateWindow {
    queue: EventQueue<WinState>,
    qh: QueueHandle<WinState>,
    state: WinState,
    layer_surface: Option<LayerSurface>,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    /// memfd fd、mmap 指针与映射长度
    map: Option<(i32, *mut u8, usize)>,
    renderer: Renderer,
}

impl CandidateWindow {
    /// 连接 compositor，burst 绑定 layer_shell + wl_shm
    /// 用于 --show-test：无活动 IM 也能跑
    pub fn new_layer() -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<WinState>(&conn)?;
        let qh = queue.handle();
        let mut state = WinState {
            layer_shell: None,
            shm: None,
            compositor: None,
            configure_serial: None,
            configured: false,
            closed: false,
        };

        globals.contents().with_list(|list| {
            for g in list {
                match g.interface.as_str() {
                    "zwlr_layer_shell_v1" if g.version >= 1 => {
                        let shell = globals
                            .registry()
                            .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, WinState>(
                                g.name,
                                1,
                                &qh,
                                (),
                            );
                        state.layer_shell = Some(shell);
                    }
                    "wl_compositor" if g.version >= 4 => {
                        state.compositor =
                            Some(globals.registry().bind::<WlCompositor, _, WinState>(
                                g.name,
                                1.min(g.version),
                                &qh,
                                (),
                            ));
                    }
                    "wl_shm" if g.version >= 1 => {
                        let shm = globals.registry().bind::<wl_shm::WlShm, _, WinState>(
                            g.name,
                            1,
                            &qh,
                            (),
                        );
                        state.shm = Some(shm);
                    }
                    _ => {}
                }
            }
        });

        if state.layer_shell.is_none() {
            return Err("compositor 未暴露 zwlr_layer_shell_v1".into());
        }
        if state.shm.is_none() {
            return Err("compositor 未暴露 wl_shm".into());
        }

        Ok(Self {
            queue,
            qh,
            state,
            layer_surface: None,
            surface: None,
            buffer: None,
            map: None,
            renderer: Renderer::new(),
        })
    }

    /// 创建基于 input_popup 角色的候选窗（新协议路径）
    /// 使用 ZwpInputMethodV2::get_input_popup_surface 直接绑定，无需 configure 握手
    /// 保持现有 shm buffer + render 管线
    pub fn new_popup(
        conn: &Connection,
        im: &ZwpInputMethodV2,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (globals, queue) = registry_queue_init::<WinState>(&conn)?;
        let qh = queue.handle();
        let mut state = WinState {
            layer_shell: None,
            shm: None,
            compositor: None,
            configure_serial: None,
            configured: false,
            closed: false,
        };

        globals.contents().with_list(|list| {
            for g in list {
                match g.interface.as_str() {
                    "wl_compositor" if g.version >= 4 => {
                        state.compositor =
                            Some(globals.registry().bind::<WlCompositor, _, WinState>(
                                g.name,
                                g.version.min(4),
                                &qh,
                                (),
                            ));
                    }
                    "wl_shm" if g.version >= 1 => {
                        let shm = globals.registry().bind::<wl_shm::WlShm, _, WinState>(
                            g.name,
                            1,
                            &qh,
                            (),
                        );
                        state.shm = Some(shm);
                    }
                    _ => {}
                }
            }
        });
        if state.compositor.is_none() {
            return Err("compositor 未暴露".into());
        }
        if state.shm.is_none() {
            return Err("shm 未暴露".into());
        }

        let compositor = state.compositor.as_ref().unwrap();
        let surface = compositor.create_surface(&qh, ());
        // 通过 IME 连接获取 input_popup_surface
        let layer_surface = im.get_input_popup_surface(&surface, &qh, ()) as _;
        // 直接设置角色，无需 configure 握手
        surface.commit();

        let renderer = Renderer::new();
        // 初始化缓冲区（与 layer-shell 路径共享 create_buffer，保证 buffer/mmap 同源）
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let shm = state.shm.as_ref().unwrap();
        let (buffer, map) = Self::create_buffer(shm, &qh, w, h)?;

        Ok(Self {
            queue,
            qh,
            state,
            layer_surface: Some(LayerSurface::Popup(layer_surface)),
            surface: Some(surface),
            buffer: Some(buffer),
            map: Some(map),
            renderer,
        })
    }

    pub fn show(
        &mut self,
        candidates: &[Candidate],
        highlight: usize,
        preedit: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        if self.layer_surface.is_none() {
            self.create_layer_surface()?;
        }
        for _ in 0..8 {
            if self.state.configured {
                break;
            }
            self.queue.roundtrip(&mut self.state)?;
        }
        if let (Some(LayerSurface::Layer(ls)), Some(serial)) =
            (&self.layer_surface, self.state.configure_serial)
        {
            ls.ack_configure(serial);
        }

        self.ensure_buffer(w, h)?;
        {
            let (_, ptr, len) = self.map.as_ref().unwrap();
            let slice = unsafe { std::slice::from_raw_parts_mut(*ptr, *len) };
            self.renderer
                .draw_candidates(slice, w, h, candidates, highlight, preedit)?;
        }
        let surface = self.surface.as_ref().ok_or("surface missing")?;
        surface.attach(Some(self.buffer.as_ref().unwrap()), 0, 0);
        surface.damage(0, 0, WIDTH as i32, HEIGHT as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(s) = &self.surface {
            s.attach(None, 0, 0);
            s.commit();
            let _ = self.queue.roundtrip(&mut self.state);
        }
        Ok(())
    }

    fn create_layer_surface(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let layer_shell = self
            .state
            .layer_shell
            .as_ref()
            .ok_or("layer shell not bound")?;
        let compositor = self
            .state
            .compositor
            .as_ref()
            .ok_or("wl_compositor not bound")?;
        let surface = compositor.create_surface(&self.qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            String::from("kime-candidates"),
            &self.qh,
            (),
        );
        layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::Bottom);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_margin(0, 0, 10, 0);
        layer_surface.set_size(WIDTH, HEIGHT);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(LayerSurface::Layer(layer_surface));
        Ok(())
    }

    fn ensure_buffer(&mut self, w: usize, h: usize) -> Result<(), Box<dyn std::error::Error>> {
        let need = w * h * 4;
        if let Some((_, _, len)) = self.map {
            if len >= need {
                return Ok(());
            }
        }
        let shm = self.state.shm.as_ref().ok_or("shm 未暴露")?;
        let (buffer, map) = Self::create_buffer(shm, &self.qh, w, h)?;
        self.buffer = Some(buffer);
        self.map = Some(map);
        Ok(())
    }

    /// 建一块 shm 缓冲：**buffer 与返回的 mmap 指针必须指向同一个 memfd**，
    /// 否则渲染写入的像素合成器永远读不到。
    fn create_buffer(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<WinState>,
        w: usize,
        h: usize,
    ) -> Result<(WlBuffer, (i32, *mut u8, usize)), Box<dyn std::error::Error>> {
        let size = w * h * 4;
        let fd = unsafe {
            libc::memfd_create(
                b"kime-candidates\0".as_ptr() as *const i8,
                libc::MFD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err("memfd_create failed".into());
        }
        if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
            unsafe { libc::close(fd) };
            return Err("ftruncate failed".into());
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return Err("mmap failed".into());
        }
        let fd_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let pool = shm.create_pool(fd_borrowed, size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888, // dwl 系广泛支持
            qh,
            (),
        );
        Ok((buffer, (fd, ptr as *mut u8, size)))
    }
}

impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, GlobalListContents> for WinState {
    fn event(
        _state: &mut Self,
        _registry: &wayland_client::protocol::wl_registry::WlRegistry,
        _event: <wayland_client::protocol::wl_registry::WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for WinState {
    fn event(
        _state: &mut Self,
        _shm: &wl_shm::WlShm,
        _event: <wl_shm::WlShm as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for WinState {
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

impl Dispatch<WlCompositor, ()> for WinState {
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

impl Dispatch<WlSurface, ()> for WinState {
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

impl Dispatch<WlBuffer, ()> for WinState {
    fn event(
        _state: &mut Self,
        _buffer: &WlBuffer,
        _event: <WlBuffer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for WinState {
    fn event(
        _state: &mut Self,
        _shell: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _event: <zwlr_layer_shell_v1::ZwlrLayerShellV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for WinState {
    fn event(
        state: &mut Self,
        _surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: <zwlr_layer_surface_v1::ZwlrLayerSurfaceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_layer_surface_v1::Event as LsEvent;
        match event {
            LsEvent::Configure { serial, .. } => {
                state.configure_serial = Some(serial);
                state.configured = true;
            }
            LsEvent::Closed => {
                state.closed = true;
                state.configured = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for WinState {
    fn event(
        _state: &mut Self,
        _surface: &ZwpInputPopupSurfaceV2,
        _event: <ZwpInputPopupSurfaceV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // input-popup-surface 的定位由合成器负责，TextInputRectangle 仅作通知，
        // 我们无需据此摆放窗口，故此 Dispatch 无副作用。
    }
}

// AsRawFd 供后续 debug/dump 使用；保留 import 合法性
#[allow(dead_code)]
fn _fd_alive(fd: &impl AsRawFd) -> i32 {
    fd.as_raw_fd()
}
