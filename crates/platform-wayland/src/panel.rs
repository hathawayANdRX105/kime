//! egui 候选窗进程。IME 通过 Unix datagram 推 JSON。

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
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

pub fn send(msg: &PanelMsg) {
    let path = socket_path();
    if let Ok(sock) = UnixDatagram::unbound() {
        let _ = sock.set_nonblocking(true);
        if let Ok(bytes) = serde_json::to_vec(msg) {
            let _ = sock.send_to(&bytes, &path);
        }
    }
}

pub fn send_hide() {
    send(&PanelMsg::default());
}

struct PanelApp {
    sock: UnixDatagram,
    state: Arc<Mutex<PanelMsg>>,
}

impl PanelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let sock = UnixDatagram::bind(&path).expect("bind kime-panel.sock");
        let _ = sock.set_nonblocking(true);
        Self {
            sock,
            state: Arc::new(Mutex::new(PanelMsg::default())),
        }
    }

    fn poll(&mut self) {
        let mut buf = [0u8; 8192];
        while let Ok((n, _)) = self.sock.recv_from(&mut buf) {
            if let Ok(msg) = serde_json::from_slice::<PanelMsg>(&buf[..n]) {
                if let Ok(mut g) = self.state.lock() {
                    *g = msg;
                }
            }
        }
    }
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

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        let msg = self.state.lock().map(|g| g.clone()).unwrap_or_default();
        let visible = !msg.candidates.is_empty() || !msg.preedit.is_empty();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
        if !visible {
            return;
        }

        let n = msg.candidates.len().min(9);
        let h = 36.0 + n as f32 * 28.0 + 16.0;
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(420.0, h)));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(30, 30, 38))
                    .inner_margin(8.0),
            )
            .show(ctx, |ui| {
                if !msg.preedit.is_empty() {
                    ui.label(
                        egui::RichText::new(&msg.preedit)
                            .size(16.0)
                            .color(Color32::from_rgb(180, 190, 210)),
                    );
                    ui.add_space(4.0);
                }
                for (i, text) in msg.candidates.iter().take(9).enumerate() {
                    let line = format!("{}. {text}", i + 1);
                    let color = if i == msg.highlight {
                        Color32::from_rgb(255, 220, 120)
                    } else {
                        Color32::from_rgb(240, 240, 245)
                    };
                    ui.label(egui::RichText::new(line).size(18.0).color(color));
                }
            });
    }
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("kime-panel")
            .with_app_id("kime-panel")
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(false)
            .with_inner_size([420.0, 220.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "kime-panel",
        options,
        Box::new(|cc| Ok(Box::new(PanelApp::new(cc)))),
    )
}
