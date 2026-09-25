//! X11 候选窗：真 override-redirect 窗口 + ZPixmap 像素上传。
//!
//! 负责候选窗口的创建、显示、隐藏、光标跟随定位与屏幕边界钳位。
//! 渲染本身在 `render.rs`（纯计算，产出 ARGB8888 字节缓冲），本模块只做
//! X11 资源管理和把 `RenderedFrame` 塞进 PutImage。
//!
//! 光标跟随的数据源是 XIM input context 的 spot-location（见 `xim.rs`），
//! 由 `set_spot` 把客户端窗口相对坐标 translate 成 root 坐标。
//!
//! 连接策略：与 XIM 事件循环共用同一个 `Arc<RustConnection>`。本模块只发
//! 请求（create/configure/put_image/map/unmap/flush），绝不调用
//! `wait_for_event`，因此与 XIM 主循环不存在并发读连接的问题。
//! （`translate_coordinates` 的 `reply()` 会顺带排空事件到 x11rb 内部队列，
//! 由后续 `wait_for_event` 正常取走，这是 x11rb 支持的用法。）

use std::error::Error;
use std::sync::Arc;

use kime_core::Candidate;
use x11rb::connection::Connection;
use x11rb::errors::ConnectionError;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt, CreateGCAux, CreateWindowAux, ImageFormat, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::render::{RenderedFrame, Renderer};

/// 候选窗左缘相对光标点的水平偏移（不要贴着光标）
pub const SPOT_DX: i32 = 5;
/// 候选窗顶边相对光标点的垂直偏移（约一行高度，把候选条放在光标所在行下方）
pub const SPOT_DY: i32 = 18;
/// 下溢翻到光标上方时，候选窗底边与光标点的净空（一行高度，避免压住光标行）
pub const SPOT_UP_CLEARANCE: i32 = 22;

/// 窗口当前几何：只有变化时才需要重新 configure
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Geometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// 纯函数：把候选窗摆进屏幕。
///
/// - 默认放在光标点右下（`SPOT_DX`/`SPOT_DY`）
/// - 右溢出：整体左推到屏幕右缘对齐
/// - 下溢出：翻到光标上方（底边离光标点 `SPOT_UP_CLEARANCE`）
/// - 超大窗口（比屏幕还大）：安全退化到贴左上角，保证不越出屏幕
/// - 光标点本身在屏外：结果仍在屏幕内
///
/// 不碰任何 X11 资源，`tests/candidate_window.rs` 直接断言。
pub fn place_window(spot: (i32, i32), size: (u32, u32), screen: (u32, u32)) -> (i32, i32) {
    let (w, h) = (size.0 as i64, size.1 as i64);
    let (sw, sh) = (screen.0 as i64, screen.1 as i64);

    // 横向：默认在光标右侧；右缘溢出则左推；推过头（超大窗口）则贴左边。
    let mut x = spot.0 as i64 + SPOT_DX as i64;
    if x + w > sw {
        x = sw - w;
    }
    if x < 0 {
        x = 0;
    }

    // 纵向：默认在光标下方；下缘溢出则翻到光标上方。光标本身可能
    // 已在屏外，因此翻转后仍需钳位到最终可见区间。
    let mut y = spot.1 as i64 + SPOT_DY as i64;
    if y + h > sh {
        y = spot.1 as i64 - SPOT_UP_CLEARANCE as i64 - h;
    }
    y = y.clamp(0, (sh - h).max(0));

    (x as i32, y as i32)
}

pub struct CandidateWindow {
    conn: Arc<RustConnection>,
    root: u32,
    /// 屏幕逻辑尺寸，钳位用（RANDR 分辨率变化后需重建窗口才更新，见下方 ponytail）
    screen_size: (u32, u32),
    win: u32,
    gc: u32,
    depth: u8,
    /// 候选窗锚点（root 坐标），来自 XIM spot-location
    spot: (i32, i32),
    geometry: Geometry,
    mapped: bool,
    renderer: Renderer,
}

impl CandidateWindow {
    /// 创建候选窗配套资源。失败时上层应容错（候选窗不可用时 preedit 仍可用）。
    ///
    /// ponytail: 窗口在启动时一次性创建，不按帧重建；代价是分辨率变化后
    /// `screen_size` 会过时导致钳位不准，届时 IM 重启即可，不值得引入 RANDR 监听。
    pub fn new(conn: Arc<RustConnection>, screen_num: usize) -> Result<Self, Box<dyn Error>> {
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let screen_size = (
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );
        let depth = screen.root_depth;

        let win = conn.generate_id()?;
        // override_redirect：不被 WM 接管，位置完全由 IM 控制，也不抢焦点。
        // save_under：移动时由 X server 自动恢复底图，省去手动重绘下层窗口。
        // 事件掩码清空：候选窗只输出不输入，也不参与 WM 焦点传递。
        conn.create_window(
            depth,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux {
                override_redirect: Some(1),
                save_under: Some(1),
                ..Default::default()
            },
        )?;

        let gc = conn.generate_id()?;
        // graphics_exposures=0：put_image 走完整像素路径，不产生 GraphicsExpose 事件。
        conn.create_gc(
            gc,
            win,
            &CreateGCAux {
                graphics_exposures: Some(0),
                ..Default::default()
            },
        )?;
        conn.flush()?;

        Ok(Self {
            conn,
            root,
            screen_size,
            win,
            gc,
            depth,
            spot: (0, 0),
            geometry: Geometry::default(),
            mapped: false,
            renderer: Renderer::new(),
        })
    }

    /// 从 XIM input context 的 spot-location 更新锚点。
    /// `spot` 是相对 `client_win` 的坐标，内部 translate 成 root 坐标。
    /// 幂等：坐标没变就不发任何 X 请求。
    pub fn set_spot(&mut self, spot_x: i32, spot_y: i32, client_win: u32) {
        if client_win == 0 || client_win == self.root {
            self.move_to(spot_x, spot_y);
            return;
        }

        // 同步取得 root 坐标；��先把结果存成值，让 connection cookie 的借用在
        // 调用 move_to 前结束。
        let translated = self
            .conn
            .translate_coordinates(client_win, self.root, spot_x as i16, spot_y as i16)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| (i32::from(reply.dst_x), i32::from(reply.dst_y)))
            .unwrap_or((spot_x, spot_y));
        self.move_to(translated.0, translated.1);
    }

    /// 直接设置 root 坐标锚点。幂等：未变化时不发请求。
    pub fn move_to(&mut self, x: i32, y: i32) {
        if self.spot == (x, y) {
            return;
        }
        self.spot = (x, y);
    }

    /// 按当前候选列表渲染并显示候选窗。无候选/隐藏帧 → 隐藏窗口。
    /// `highlight` 为高亮项在 `candidates` 中的下标（不含头部模式字）。
    /// `chinese` 为当前中英模式，决定头部常驻模式字「中」/「英」。
    pub fn show_candidates(
        &mut self,
        candidates: &[Candidate],
        highlight: usize,
        chinese: bool,
    ) -> Result<(), ConnectionError> {
        if candidates.is_empty() {
            self.hide();
            return Ok(());
        }
        self.renderer.set_candidates(candidates);
        self.renderer.set_chinese(chinese);
        self.renderer.set_highlight(highlight);
        let frame = self.renderer.render();
        if frame.is_hidden() {
            self.hide();
            return Ok(());
        }
        self.show_at(self.spot.0, self.spot.1, &frame)
    }

    /// 把 `frame` 摆到锚点附近（钳位后）并上传显示。几何未变化时跳过 configure。
    pub fn show_at(
        &mut self,
        x: i32,
        y: i32,
        frame: &RenderedFrame,
    ) -> Result<(), ConnectionError> {
        // 尺寸守卫：put_image 的宽高参数是 u16，超限会把 65536 回绕成 0、
        // 打出协议错误打死整条 X 连接（issue #48）。render() 出口已钳 8192，
        // 这里是防"绕过 render() 直接喂帧"的最后一道（show_at 是 pub）。
        // 越界帧跳过上传并告警，XIM 进程继续活着。
        if frame.width > u16::MAX as u32 || frame.height > u16::MAX as u32 {
            eprintln!(
                "[platform-x11] 候选帧 {}x{} 超出 u16，跳过上传（width 应已钳 8192，这里越界说明上游钳位失效）",
                frame.width, frame.height
            );
            return Ok(());
        }
        let (px, py) = place_window((x, y), (frame.width, frame.height), self.screen_size);
        let next = Geometry {
            x: px,
            y: py,
            width: frame.width,
            height: frame.height,
        };

        if self.geometry != next {
            // 候选条宽度不会超过 u16；真要超了，截断也比 panic 强
            self.conn.configure_window(
                self.win,
                &ConfigureWindowAux {
                    x: Some(px),
                    y: Some(py),
                    width: Some(frame.width),
                    height: Some(frame.height),
                    ..Default::default()
                },
            )?;
            self.geometry = next;
        }

        // ZPixmap + 与窗口同深度：小端机器上 ARGB8888 字节序 B,G,R,A，
        // 深度 24 时第 4 字节是 pad，正好对上 render.rs 的缓冲布局。
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.win,
            self.gc,
            frame.width as u16,
            frame.height as u16,
            0,
            0,
            0,
            self.depth,
            &frame.pixels,
        )?;

        if !self.mapped {
            self.conn.map_window(self.win)?;
            self.mapped = true;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// 隐藏候选窗。幂等：已隐藏时零请求。
    pub fn hide(&mut self) {
        self.renderer.set_visible(false);
        if self.mapped {
            let _ = self.conn.unmap_window(self.win);
            let _ = self.conn.flush();
            self.mapped = false;
        }
    }

    /// 候选窗是否已 map（测试/诊断用）
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// 当前锚点（root 坐标，测试/诊断用）
    pub fn spot(&self) -> (i32, i32) {
        self.spot
    }
}
