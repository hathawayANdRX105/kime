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
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::render::{Candidate, Renderer};

const WIDTH: u32 = 420;
const HEIGHT: u32 = 380;

struct WinState {
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    shm: Option<wl_shm::WlShm>,
    compositor: Option<WlCompositor>,
    configure_serial: Option<u32>,
    configured: bool,
    closed: bool,
}

pub struct CandidateWindow {
    conn: Connection,
    queue: EventQueue<WinState>,
    qh: QueueHandle<WinState>,
    state: WinState,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    /// memfd fd、mmap 指针与映射长度
    map: Option<(i32, *mut u8, usize)>,
    renderer: Renderer,
}

impl CandidateWindow {
    /// 连接 compositor，burst 绑定 layer_shell + wl_shm
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
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
            conn,
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

    /// 显示候选窗（建窗 + 渲染 + 上屏），自轮询 configure 握手
    pub fn show(
        &mut self,
        candidates: &[Candidate],
        highlight: usize,
        preedit: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.layer_surface.is_none() {
            self.create_layer_surface()?;
        }
        // 等 configure（最多 3 轮 roundtrip）
        for _ in 0..3 {
            if self.state.configured {
                break;
            }
            self.queue.roundtrip(&mut self.state)?;
        }
        let serial = self
            .state
            .configure_serial
            .ok_or("layer surface configure 未到达")?;
        self.layer_surface.as_ref().unwrap().ack_configure(serial);

        // 渲染到 mmap → attach → commit
        if self.layer_surface.is_none() {
            self.create_layer_surface()?;
        }
        // 等 configure（最多 5 轮 roundtrip）
        for i in 0..5 {
            if self.state.configured {
                break;
            }
            self.queue.roundtrip(&mut self.state)?;
            eprintln!("[win] roundtrip {} configured={}", i, self.state.configured);
        }
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        self.ensure_buffer(w, h)?;
        // render.rs 写内存 RGBA；shm Argb8888 内存序是 B,G,R,A → 原地换 R/B
        {
            let (_, ptr, len) = self.map.as_ref().unwrap();
            let slice = unsafe { std::slice::from_raw_parts_mut(*ptr, *len) };
            self.renderer
                .draw_candidates(slice, w, h, candidates, highlight, preedit)?;
            for px in slice.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(self.buffer.as_ref().unwrap()), 0, 0);
        surface.damage(0, 0, WIDTH as i32, HEIGHT as i32);
        surface.commit();
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    /// 隐藏候选窗（销毁 layer surface，保留连接与缓冲）
    pub fn hide(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ls) = &self.layer_surface {
            ls.destroy();
        }
        if let Some(s) = &self.surface {
            s.destroy();
        }
        self.queue.roundtrip(&mut self.state)?;
        self.layer_surface = None;
        self.surface = None;
        self.state.configured = false;
        self.state.configure_serial = None;
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
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
        Ok(())
    }

    /// 首次调用：memfd + ftruncate + mmap + wl_shm pool + buffer
    fn ensure_buffer(&mut self, w: usize, h: usize) -> Result<(), Box<dyn std::error::Error>> {
        if self.buffer.is_some() {
            return Ok(());
        }
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
            return Err("mmap failed".into());
        }
        let map_ptr = ptr as *mut u8;
        let shm = self.state.shm.as_ref().unwrap();
        let fd_borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let pool = shm.create_pool(fd_borrowed, size as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            wl_shm::Format::Argb8888, // dwl 系广泛支持
            &self.qh,
            (),
        );
        self.buffer = Some(buffer);
        self.map = Some((fd, map_ptr, size));
        Ok(())
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
        state: &mut Self,
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
        state: &mut Self,
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
        state: &mut Self,
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
        state: &mut Self,
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

// AsRawFd 供后续 debug/dump 使用；保留 import 合法性
#[allow(dead_code)]
fn _fd_alive(fd: &impl AsRawFd) -> i32 {
    fd.as_raw_fd()
}
