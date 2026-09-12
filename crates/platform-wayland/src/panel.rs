//! egui 候选窗进程。IME 通过 Unix datagram 推 JSON。
//! socket 读取在后台线程：mango 对透明空闲帧停发 frame-done 时渲染循环会卡在 swap，
//! 若在 update() 里 poll，面板会对 IME 消息聋 20s~8min（实测）。

use std::io::{Read, Write};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontTweak, Vec2};
use serde::{Deserialize, Serialize};

/// 光标矩形：IME 把 `text_input_rectangle`（surface local）加上焦点窗口原点，
/// 换算成绝对屏幕坐标后随消息带来。面板只关心「落在哪儿」，钳制在 place_cursor 里算。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct CaretRect {
    /// 光标行左上角（绝对屏幕坐标）
    pub x: i32,
    pub y: i32,
    /// 光标行高：面板默认贴在下沿
    pub height: i32,
    /// 光标所在屏幕的绝对边界，越界钳制用
    pub screen_x: i32,
    pub screen_y: i32,
    pub screen_w: i32,
    pub screen_h: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PanelMsg {
    #[serde(default)]
    pub preedit: String,
    #[serde(default)]
    pub highlight: usize,
    #[serde(default)]
    pub candidates: Vec<String>,
    /// None = 没拿到光标位置（旧版 IME / 非 mango 会话 / IPC 失败）→ 面板保持原位。
    /// 缺字段时 serde 也给 None，所以老消息照样能解。
    #[serde(default)]
    pub cursor: Option<CaretRect>,
}

/// 面板左上角落点：贴光标行下方；右边超出屏幕则左移贴边，下方放不下则翻到光标上方。
/// 纯计算，不碰 wayland/egui/IPC —— tests/panel_placement.rs 直接断言边界。
pub fn place_cursor(c: &CaretRect, panel_w: f32, panel_h: f32) -> (f32, f32) {
    let (left, top) = (c.screen_x as f32, c.screen_y as f32);
    let (right, bottom) = (
        left + c.screen_w.max(0) as f32,
        top + c.screen_h.max(0) as f32,
    );
    let mut x = c.x as f32;
    if x + panel_w > right {
        x = right - panel_w;
    }
    let mut y = c.y as f32 + c.height.max(0) as f32;
    if y + panel_h > bottom {
        y = c.y as f32 - panel_h;
    }
    // 面板比屏幕还宽/还高时上式会把落点推出边界，最后夹回屏幕内
    (
        x.clamp(left, right.max(left)),
        y.clamp(top, bottom.max(top)),
    )
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

// ---- mango IPC ---------------------------------------------------------------
// `MANGO_INSTANCE_SIGNATURE` 就是 socket 路径（mmsg 也读它）。协议：一行 JSON 命令
// → 一行 JSON 回答，直连实测 ~0.1ms。绝不 fork mmsg：每次按键 spawn 一个进程太贵。
// 任何失败（非 mango 会话 / compositor 卡住）都返回 None，调用方退化成不挪窗口。

fn mango_ipc(cmd: &str) -> Option<serde_json::Value> {
    let path = std::env::var("MANGO_INSTANCE_SIGNATURE").ok()?;
    let mut s = UnixStream::connect(path).ok()?;
    // 双向 50ms 死线：这条路径在按键线程上，宁可拿不到坐标也不能卡住输入
    let timeout = Some(Duration::from_millis(50));
    let _ = s.set_write_timeout(timeout);
    let _ = s.set_read_timeout(timeout);
    s.write_all(cmd.as_bytes()).ok()?;
    s.write_all(b"\n").ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') || buf.len() > 1 << 20 {
                    break;
                }
            }
        }
    }
    serde_json::from_slice(&buf).ok()
}

fn json_i32(v: &serde_json::Value, key: &str) -> i32 {
    v.get(key).and_then(|n| n.as_i64()).unwrap_or(0) as i32
}

/// 绝对几何：x, y, w, h。width/height 为 0 = 关掉的屏（eDP 只用外接时就是 0）或查询失败。
fn json_rect(v: &serde_json::Value) -> Option<(i32, i32, i32, i32)> {
    let (w, h) = (json_i32(v, "width"), json_i32(v, "height"));
    (w > 0 && h > 0).then(|| (json_i32(v, "x"), json_i32(v, "y"), w, h))
}

/// 焦点窗口（surface-local 光标 rect 的平移原点）+ 它所在的屏幕名。
pub struct FocusWin {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub monitor: Option<String>,
}

/// 焦点窗口几何。回答里没有 error 才算成功。
pub fn mango_focusing_client() -> Option<FocusWin> {
    let v = mango_ipc("get focusing-client")?;
    if v.get("error").is_some() {
        return None;
    }
    Some(FocusWin {
        x: json_i32(&v, "x"),
        y: json_i32(&v, "y"),
        w: json_i32(&v, "width"),
        h: json_i32(&v, "height"),
        monitor: v.get("monitor").and_then(|m| m.as_str()).map(str::to_owned),
    })
}

/// 光标所在屏幕的边界。`mango get monitor <name>` 失败时退回第一块 active 屏。
pub fn mango_screen(monitor: Option<&str>) -> Option<(i32, i32, i32, i32)> {
    if let Some(name) = monitor {
        if let Some(r) = mango_ipc(&format!("get monitor {name}"))
            .as_ref()
            .and_then(json_rect)
        {
            return Some(r);
        }
    }
    let all = mango_ipc("get all-monitors")?;
    all.get("monitors")?
        .as_array()?
        .iter()
        .find(|m| m.get("active").and_then(|a| a.as_bool()) == Some(true))
        .and_then(json_rect)
}

/// 本进程自己的窗口：(client id, 绝对 x, y, w, h)。按 pid 认领，别拿 appid 猜。
/// mango 对不存在的 client id 会把 dispatch 落到**当前焦点窗口**，所以 id 只能现查、
/// 绝不能用缓存 —— 查错就是把用户的窗口搬走。
fn mango_self() -> Option<(i64, i32, i32, i32, i32)> {
    let pid = i64::from(std::process::id());
    let c = mango_ipc("get all-clients")?
        .get("clients")?
        .as_array()?
        .iter()
        .find(|c| c.get("pid").and_then(|p| p.as_i64()) == Some(pid))?
        .clone();
    Some((
        c.get("id")?.as_i64()?,
        json_i32(&c, "x"),
        json_i32(&c, "y"),
        json_i32(&c, "width"),
        json_i32(&c, "height"),
    ))
}

/// 面板贴到光标旁。落点尺寸问合成器拿真实值 —— 面板自己的 `last_size` 可能还是
/// 上一帧内容的尺寸，钳制会偏。非 mango 会话查不到 → 退化成不钳制。
fn follow_cursor(ctx: &egui::Context, cur: &CaretRect) {
    let me = mango_self();
    let (x, y) = match me {
        Some((_, _, _, w, h)) => place_cursor(cur, w as f32, h as f32),
        None => (cur.x as f32, (cur.y + cur.height.max(0)) as f32),
    };
    // 与现状一致就一条命令都不发：每帧刷位置会抖，也白给合成器加 damage
    if let Some((_, cx, cy, _, _)) = me {
        if (x - cx as f32).abs() < 0.5 && (y - cy as f32).abs() < 0.5 {
            return;
        }
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
    if let Some((id, _, _, _, _)) = me {
        // 实测 mango 0.16.1：dispatch movewin 的参数是绝对屏幕坐标。
        // winit 的 set_outer_position 在 Wayland 上是空实现（协议不允许 toplevel
        // 自定位），所以真正搬窗的是这条。
        mango_ipc(&format!(
            "dispatch movewin,{},{} client,{id}",
            x as i32, y as i32
        ));
    }
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
/// 挪窗也在这条线程上：窗口不在当前 tag / 全透明帧卡住 swap 时 update() 可能几十秒
/// 不跑一次，放 update() 里面板就会按旧坐标显示。
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
                        if let Some(cur) = msg.cursor {
                            follow_cursor(&ctx, &cur);
                        }
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
