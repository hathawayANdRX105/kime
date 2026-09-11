//! egui 候选窗进程。IME 通过 Unix datagram 推 JSON。
//! socket 读取在后台线程：mango 对透明空闲帧停发 frame-done 时渲染循环会卡在 swap，
//! 若在 update() 里 poll，面板会对 IME 消息聋 20s~8min（实测）。

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontTweak, Vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PanelMsg {
    #[serde(default)]
    pub preedit: String,
    #[serde(default)]
    pub highlight: usize,
    #[serde(default)]
    pub candidates: Vec<String>,
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(dir).join("kime-panel.sock")
}

/// 发送失败只吼一次（限流）：僵尸 panel/漏 unlink 时 ECONNREFUSED 全吞会让 IME 永久没候选窗。
static SEND_ERR_LOGGED: AtomicBool = AtomicBool::new(false);

pub fn send(msg: &PanelMsg) {
    let path = socket_path();
    let sock = match UnixDatagram::unbound() {
        Ok(s) => s,
        Err(e) => return log_send_err(&path, &format!("socket: {e}")),
    };
    let bytes = match serde_json::to_vec(msg) {
        Ok(b) => b,
        Err(e) => return log_send_err(&path, &format!("encode: {e}")),
    };
    if let Err(e) = sock.send_to(&bytes, &path) {
        log_send_err(&path, &format!("send_to: {e}"));
    }
}

fn log_send_err(path: &Path, why: &str) {
    if !SEND_ERR_LOGGED.swap(true, Ordering::Relaxed) {
        eprintln!("[kime-panel] send to {} failed: {why}", path.display());
    }
}

pub fn send_hide() {
    send(&PanelMsg::default());
}

struct PanelApp {
    state: Arc<Mutex<PanelMsg>>,
    last_size: Vec2,
    /// 空闲心跳像素的交替坐标：每帧制造 1px 真实 damage，
    /// 防 mango 对全透明帧停发 frame-done（旧聋区根因）。
    tick: u32,
}

impl PanelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let sock = UnixDatagram::bind(&path).expect("bind kime-panel.sock");
        let state = Arc::new(Mutex::new(PanelMsg::default()));
        spawn_reader(sock, path, state.clone(), cc.egui_ctx.clone());
        Self {
            state,
            last_size: Vec2::new(420.0, 48.0),
            tick: 0,
        }
    }
}
/// 后台读 socket：阻塞 recv 与渲染循环解耦，渲染卡死也永不漏消息。
fn spawn_reader(
    sock: UnixDatagram,
    path: PathBuf,
    state: Arc<Mutex<PanelMsg>>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match sock.recv(&mut buf) {
                Ok(n) => {
                    if let Ok(msg) = serde_json::from_slice::<PanelMsg>(&buf[..n]) {
                        if let Ok(mut g) = state.lock() {
                            *g = msg;
                        }
                        ctx.request_repaint();
                    }
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    eprintln!("[kime-panel] socket recv 结束: {e}");
                    break;
                }
            }
        }
        // 读线程只在 socket 出错或进程退出时停：unlink 兜底僵尸 socket，
        // 发送端连 ECONNREFUSED 时至少日志可见（见 send 的限流告警）。
        let _ = std::fs::remove_file(&path);
    });
}

fn install_cjk_font(ctx: &egui::Context) {
    let ttc = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc";
    let Ok(bytes) = std::fs::read(ttc) else {
        eprintln!("[kime-panel] no CJK font at {ttc}");
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto-cjk".into(),
        std::sync::Arc::new(FontData {
            font: std::borrow::Cow::Owned(bytes),
            index: 0,
            tweak: FontTweak::default(),
        }),
    );
    if let Some(names) = fonts.families.get_mut(&FontFamily::Proportional) {
        names.insert(0, "noto-cjk".into());
    }
    if let Some(names) = fonts.families.get_mut(&FontFamily::Monospace) {
        names.insert(0, "noto-cjk".into());
    }
    ctx.set_fonts(fonts);
}

const DEFAULT_SIZE: Vec2 = Vec2::new(420.0, 48.0);

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let msg = self.state.lock().map(|g| g.clone()).unwrap_or_default();
        // preedit 由应用内联显示（set_preedit_string），面板只画候选
        let visible = !msg.candidates.is_empty();
        // 空闲降帧：30fps 轮询，不常驻 60fps 烧 CPU/GPU
        let interval = if visible { 16 } else { 33 };
        ctx.request_repaint_after(std::time::Duration::from_millis(interval));

        const MARGIN_X: f32 = 10.0;
        const MARGIN_Y: f32 = 6.0;

        if !visible {
            // 空闲：透明帧 + 1px alpha=1 心跳像素（交替位置）。窗口保持原尺寸，
            // show 时零 configure 往返，下一帧（≤16ms）直接可见。
            self.tick = self.tick.wrapping_add(1);
            let px = if self.tick & 1 == 0 { 0.0 } else { 1.0 };
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(Color32::TRANSPARENT))
                .show(ctx, |ui| {
                    ui.set_min_size(DEFAULT_SIZE);
                    let p = egui::Pos2::new(px, px);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(p, Vec2::splat(1.0)),
                        0.0,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 1),
                    );
                });
            return;
        }

        let n = msg.candidates.len().min(10);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(30, 30, 38))
                    .inner_margin(egui::Margin::symmetric(MARGIN_X as i8, MARGIN_Y as i8)),
            )
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    for (i, text) in msg.candidates.iter().take(n).enumerate() {
                        let entry = format!("{}. {}", i + 1, text);
                        let label = egui::RichText::new(entry).size(16.0);
                        let label = if i == msg.highlight {
                            label.color(Color32::from_rgb(255, 220, 120))
                        } else {
                            label.color(Color32::from_rgb(240, 240, 245))
                        };
                        ui.label(label);
                        if i + 1 < n {
                            ui.add_space(10.0);
                        }
                    }
                });
            });

        // 内容自适应：高度贴合单行，只在变化时发
        let used = ctx.used_size();
        let target = Vec2::new(
            (used.x + 2.0 * MARGIN_X + 2.0).clamp(140.0, 760.0),
            (used.y + 2.0 * MARGIN_Y + 2.0).clamp(32.0, 48.0),
        );
        if (target - self.last_size).length() > 1.0 {
            self.last_size = target;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
        }
    }
    /// GL 清屏色恒为全透明；实底由可见帧自己画。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("kime-panel")
            .with_app_id("kime-panel")
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(true)
            .with_inner_size([420.0, 48.0])
            .with_resizable(false),
        ..Default::default()
    };
    let res = eframe::run_native(
        "kime-panel",
        options,
        Box::new(|cc| Ok(Box::new(PanelApp::new(cc)))),
    );
    // 正常退出路径 unlink，不留僵尸 socket 让发送端连 ECONNREFUSED
    let _ = std::fs::remove_file(socket_path());
    res
}
