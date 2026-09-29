//! XIM server for XWayland applications.
//!
//! Registers `@server=kime`, accepts XIM input contexts, routes forwarded
//! key events through `kime_core::Engine`, publishes preedit text, and commits
//! selected Chinese text back to the focused XIM client.

use std::error::Error;
use std::io::Write;
use std::num::NonZeroU32;
use std::sync::Arc;

use kime_core::{Engine, Key, Outcome};
use parking_lot::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt, EventMask, KeyButMask, KeyPressEvent, KeyReleaseEvent,
};
use x11rb::rust_connection::RustConnection;
use xim::{
    x11rb::X11rbServer, Server, ServerError, ServerHandler, UserInputContext, XimConnections,
};
use xim_parser::InputStyle;
use xkbcommon::xkb;

use crate::window::CandidateWindow;

const IM_NAME: &str = "kime";
const XIM_FORWARD_KEY_PRESS: u32 = 1;
/// X11 BackSpace 键码（#85：标准布局恒 22）。
const X11_KEYCODE_BACKSPACE: u8 = 22;

const X11_KEYCODE_OFFSET: u32 = 8;
const KEY_ESC: u32 = 1;

/// KIME_DEBUG=1 状态转移日志：X11 前端自写自己的文件 /tmp/kime-ime-debug-x11.log
///（与 wayland 前端 /tmp/kime-ime-debug.log 同形、各写各的，互不截断）。
/// 写失败静默忽略，日志不许卡键流。`X11IM::new` 启动读一次 env。
static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 惰性打开的追加写句柄（首条日志时打开；进程退出随析构 flush）。
static DEBUG_FILE: std::sync::LazyLock<
    std::sync::Mutex<Option<std::io::BufWriter<std::fs::File>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// 记一条 KIME_DEBUG 状态日志（行内容由闭包**惰性**构造：关着时零文件 I/O）。
fn debug_log(line: impl FnOnce() -> String) {
    if !DEBUG.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut guard = DEBUG_FILE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/kime-ime-debug-x11.log")
            .ok()
            .map(std::io::BufWriter::new);
    }
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{}", line());
    }
}

pub struct X11IM {
    conn: Arc<RustConnection>,
    server: X11rbServer<Arc<RustConnection>>,
    connections: XimConnections<()>,
    handler: Handler,
    running: bool,
}

impl X11IM {
    pub fn new(
        conn: Arc<RustConnection>,
        screen_num: usize,
        engine: Engine,
    ) -> Result<Self, Box<dyn Error>> {
        // KIME_DEBUG=1：状态转移日志开关，启动读一次 env（之后按键路径零 env 读取）。
        DEBUG.store(
            std::env::var_os("KIME_DEBUG").is_some(),
            std::sync::atomic::Ordering::Relaxed,
        );
        let server = X11rbServer::init(conn.clone(), screen_num, IM_NAME, xim::ALL_LOCALES)?;
        // 候选窗创建失败只降级（无候选窗、仅 preedit），不拖垮 IM 主循环。
        let window = match CandidateWindow::new(conn.clone(), screen_num) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("[platform-x11] candidate window unavailable, preedit-only: {e}");
                None
            }
        };
        let handler = Handler::new(engine, window, conn.clone());

        Ok(Self {
            conn,
            server,
            connections: XimConnections::new(),
            handler,
            running: true,
        })
    }
    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        while self.running {
            let event = self.conn.wait_for_event()?;
            self.server
                .filter_event(&event, &mut self.connections, &mut self.handler)?;
        }
        Ok(())
    }
}

struct Handler {
    engine: Arc<Mutex<Engine>>,
    keyboard: xkb::State,
    /// None = 候选窗不可用（降级到纯 preedit 模式）
    window: Option<CandidateWindow>,
    /// 最近一次 set_focus 的输入上下文 id（每个 XIM 应用一个 IC）。
    /// 同应用焦点抖动（同一 IC 反复 set/unset focus）不清撤销栈（#70 契约）；
    /// 换应用（新 IC 的 set_focus）= 真实切应用 → `engine.clear_undo()`。
    last_ic_id: Option<u16>,
    /// X11 连接（#85 标点撤销：合成退格 XSendEvent 用）。
    conn: Arc<RustConnection>,
}

impl Handler {
    fn new(engine: Engine, window: Option<CandidateWindow>, conn: Arc<RustConnection>) -> Self {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("failed to compile the default XKB keymap");

        Self {
            engine: Arc::new(Mutex::new(engine)),
            keyboard: xkb::State::new(&keymap),
            window,
            conn,
            last_ic_id: None,
        }
    }

    fn clear_composition(&self) {
        let mut engine = self.engine.lock();
        let _ = engine.key(Key {
            code: KEY_ESC,
            shift: false,
            ctrl: false,
            alt: false,
            ch: None,
        });
    }

    /// #85 标点撤销降级：向客户端窗口 XSendEvent 一对合成 BackSpace 点击
    /// （KeyPress+KeyRelease；事件缓冲首字节置 send_event 位）。个别应用
    /// 过滤合成事件时退格被忽略，但后续 commit 上屏不受影响。
    fn send_synthetic_backspace(&self, win: u32) {
        let mask = EventMask::KEY_PRESS | EventMask::KEY_RELEASE;
        let press = KeyPressEvent {
            response_type: x11rb::protocol::xproto::KEY_PRESS_EVENT | 0x01, // send_event 位
            detail: X11_KEYCODE_BACKSPACE,
            sequence: 0,
            time: 0,
            root: 0,
            event: win,
            child: 0,
            root_x: 0,
            root_y: 0,
            event_x: 0,
            event_y: 0,
            state: 0u16.into(),
            same_screen: true,
        };
        let _ = self.conn.send_event(false, win, mask, press);
        let release = KeyReleaseEvent {
            response_type: x11rb::protocol::xproto::KEY_RELEASE_EVENT | 0x01, // send_event 位
            detail: X11_KEYCODE_BACKSPACE,
            sequence: 0,
            time: 0,
            root: 0,
            event: win,
            child: 0,
            root_x: 0,
            root_y: 0,
            event_x: 0,
            event_y: 0,
            state: 0u16.into(),
            same_screen: true,
        };
        let _ = self.conn.send_event(false, win, mask, release);
    }

    fn key_from_event(&mut self, event: &KeyPressEvent) -> Key {
        let mask = u16::from(event.state) as u32;
        self.keyboard.update_mask(mask, 0, 0, 0, 0, 0);

        let xkb_keycode = xkb::Keycode::new(u32::from(event.detail));
        let ch = char::from_u32(self.keyboard.key_get_utf32(xkb_keycode))
            .filter(|value| !value.is_control() && *value != ' ');

        Key {
            code: u32::from(event.detail).saturating_sub(X11_KEYCODE_OFFSET),
            shift: event.state.contains(KeyButMask::SHIFT),
            ctrl: event.state.contains(KeyButMask::CONTROL),
            alt: event.state.contains(KeyButMask::MOD1),
            ch,
        }
    }

    /// 隐藏候选窗（生命周期终止路径：commit / reset / unset-focus / destroy）。
    fn hide_window(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.hide();
        }
    }

    /// 用当前引擎状态刷新候选窗：有候选就渲染并在锚点显示，没候选就隐藏。
    fn update_window(&mut self) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        let (candidates, highlight) = {
            let engine = self.engine.lock();
            // 按页切片（与 wayland 前端同律）：candidate_limit 默认 50，
            // 不切片会把整列候选塞进一行，面板超长占满桌面。
            let (start, page_size) = engine.page();
            let all = engine.candidates();
            let page = all[start.min(all.len())..(start + page_size).min(all.len())].to_vec();
            (page, engine.highlight().saturating_sub(start))
        };
        if candidates.is_empty() {
            window.hide();
            return;
        }
        if let Err(error) = window.show_candidates(&candidates, highlight) {
            eprintln!("[platform-x11] candidate window draw failed: {error}");
        }
    }
}

impl<S> ServerHandler<S> for Handler
where
    S: Server<XEvent = KeyPressEvent>,
{
    type InputContextData = ();
    type InputStyleArray = [InputStyle; 4];

    fn new_ic_data(
        &mut self,
        _server: &mut S,
        _input_style: InputStyle,
    ) -> Result<Self::InputContextData, ServerError> {
        Ok(())
    }

    fn input_styles(&self) -> Self::InputStyleArray {
        [
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NONE,
        ]
    }

    fn filter_events(&self) -> u32 {
        XIM_FORWARD_KEY_PRESS
    }

    fn handle_connect(&mut self, _server: &mut S) -> Result<(), ServerError> {
        Ok(())
    }

    fn handle_create_ic(
        &mut self,
        server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
        server.set_event_mask(&user_ic.ic, XIM_FORWARD_KEY_PRESS, 0)
    }
    fn handle_destroy_ic(
        &mut self,
        server: &mut S,
        mut user_ic: UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
        server.preedit_draw(&mut user_ic.ic, "")?;
        self.hide_window();
        self.clear_composition();
        // 被销毁的 IC 若正是当前焦点 IC：它的输入串已结束，撤销栈随之作废。
        let ic_id = user_ic.ic.input_context_id().get();
        if self.last_ic_id == Some(ic_id) {
            self.engine.lock().clear_undo();
            self.last_ic_id = None;
        }
        debug_log(|| format!("DESTROY_IC ic#{ic_id}"));
        Ok(())
    }

    fn handle_reset_ic(
        &mut self,
        server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<String, ServerError> {
        let preedit = self.engine.lock().preedit().to_owned();
        server.preedit_draw(&mut user_ic.ic, "")?;
        self.hide_window();
        self.clear_composition();
        Ok(preedit)
    }

    fn handle_set_focus(
        &mut self,
        server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
        // XIM 客户端共享同一个全局 Engine：应用 A 打到一半切到应用 B，
        // set_focus 到来时若不清组合，A 的拼音和候选会带到 B 的输入框。
        // 对齐 wayland 前端 Deactivate 的做法（main.rs:999-1007）：
        // 清 preedit + 隐藏候选窗 + 清引擎组合。
        server.preedit_draw(&mut user_ic.ic, "")?;
        self.hide_window();
        self.clear_composition();
        // 真实切应用 = 新输入上下文（换应用才有新 IC）：旧 burst 的撤销栈不得
        // 跨应用边界。同应用焦点抖动（同一 IC 反复 set/unset focus）不清栈（#70）。
        let ic_id = user_ic.ic.input_context_id().get();
        let switched = self.last_ic_id != Some(ic_id);
        if switched {
            self.engine.lock().clear_undo();
        }
        self.last_ic_id = Some(ic_id);
        debug_log(|| {
            format!(
                "SET_FOCUS ic#{ic_id} -> {}",
                if switched {
                    "new ic -> clear_undo"
                } else {
                    "same ic (no clear)"
                }
            )
        });
        Ok(())
    }

    fn handle_unset_focus(
        &mut self,
        server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
        server.preedit_draw(&mut user_ic.ic, "")?;
        self.hide_window();
        self.clear_composition();
        // 不动撤销栈（焦点抖动安全，#70）：last_ic_id 保留，同应用重新聚焦
        // （set_focus 同 ic id）不会误清。
        debug_log(|| format!("UNSET_FOCUS ic#{}", user_ic.ic.input_context_id().get()));
        Ok(())
    }

    /// XNSpotLocation / XNClientWindow 到达：translate 成 root 坐标并更新候选窗锚点。
    fn handle_set_ic_values(
        &mut self,
        _server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
        // 候选条相对 XNClientWindow 定位；应用没给 client window 时退到 IM 连接窗口。
        let client_win = user_ic
            .ic
            .app_win()
            .map(NonZeroU32::get)
            .unwrap_or_else(|| user_ic.ic.client_win());
        let spot = user_ic.ic.preedit_spot();
        if let Some(window) = self.window.as_mut() {
            window.set_spot(spot.x as i32, spot.y as i32, client_win);
        }
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        server: &mut S,
        user_ic: &mut UserInputContext<Self::InputContextData>,
        event: &S::XEvent,
    ) -> Result<bool, ServerError> {
        let key = self.key_from_event(event);
        let (outcome, preedit) = {
            let mut engine = self.engine.lock();
            let outcome = engine.key(key);
            // Commit 也要取剩余组合：选词只消耗已选音节时，剩余拼音转为新 preedit
            let preedit = match outcome {
                Outcome::Consumed | Outcome::Commit(_) => Some(engine.preedit().to_owned()),
                _ => None,
            };
            (outcome, preedit)
        };

        // KIME_DEBUG 状态快照（关着时闭包不执行，零开销）：键 + outcome + 引擎状态
        // {letters, preedit, cursor, undo_depth, undo_top}，与 wayland 前端同形。
        debug_log(|| {
            let engine = self.engine.lock();
            format!(
                "key code={} ch={:?} -> {:?} | letters={:?} preedit={:?} cursor={} undo=[{}, {}]",
                key.code,
                key.ch,
                outcome,
                engine.letters(),
                engine.preedit(),
                engine.cursor(),
                engine.undo_depth(),
                engine
                    .undo_top()
                    .map(|(k, c)| format!("{k}×{c}"))
                    .unwrap_or_else(|| "-".into()),
            )
        });

        match outcome {
            Outcome::Consumed => {
                if let Some(preedit) = preedit {
                    server.preedit_draw(&mut user_ic.ic, &preedit)?;
                }
                self.update_window();
                Ok(true)
            }
            Outcome::Commit(text) => {
                server.commit(&user_ic.ic, &text)?;
                // 先上屏已选词，剩余拼音（若有）转为新的组合显示——fcitx5 预选行为；
                // 无剩余则清 preedit 并收候选窗。
                match preedit {
                    Some(pe) if !pe.is_empty() => {
                        server.preedit_draw(&mut user_ic.ic, &pe)?;
                        self.update_window();
                    }
                    _ => {
                        server.preedit_draw(&mut user_ic.ic, "")?;
                        self.hide_window();
                    }
                }
                Ok(true)
            }
            Outcome::PuncCancel {
                original,
                fullwidth,
            } => {
                // #85 XIM 侧：无按键转发通道——降级 XSendEvent 合成退格点击
                // （× fullwidth 字符数，删掉上屏全角字符），再上屏原半角按键串。
                // 合成事件可能被过滤 send_event 的应用忽略，commit 不受影响。
                let win = user_ic
                    .ic
                    .app_win()
                    .map(NonZeroU32::get)
                    .unwrap_or_else(|| user_ic.ic.client_win());
                for _ in 0..fullwidth.chars().count().max(1) {
                    self.send_synthetic_backspace(win);
                }
                server.commit(&user_ic.ic, &original)?;
                server.preedit_draw(&mut user_ic.ic, "")?;
                self.hide_window();
                Ok(true)
            }
            Outcome::Ignored => Ok(false),
        }
    }
}
