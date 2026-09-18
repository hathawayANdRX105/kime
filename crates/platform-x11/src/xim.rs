//! XIM server for XWayland applications.
//!
//! Registers `@server=kime`, accepts XIM input contexts, routes forwarded
//! key events through `kime_core::Engine`, publishes preedit text, and commits
//! selected Chinese text back to the focused XIM client.

use std::error::Error;
use std::num::NonZeroU32;
use std::sync::Arc;

use kime_core::{Engine, Key, Outcome};
use parking_lot::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{KeyButMask, KeyPressEvent};
use x11rb::rust_connection::RustConnection;
use xim::{
    x11rb::X11rbServer, Server, ServerError, ServerHandler, UserInputContext, XimConnections,
};
use xim_parser::InputStyle;
use xkbcommon::xkb;

use crate::window::CandidateWindow;

const IM_NAME: &str = "kime";
const XIM_FORWARD_KEY_PRESS: u32 = 1;
const X11_KEYCODE_OFFSET: u32 = 8;
const KEY_ESC: u32 = 1;

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
        let server = X11rbServer::init(conn.clone(), screen_num, IM_NAME, xim::ALL_LOCALES)?;
        // 候选窗创建失败只降级（无候选窗、仅 preedit），不拖垮 IM 主循环。
        let window = match CandidateWindow::new(conn.clone(), screen_num) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("[platform-x11] candidate window unavailable, preedit-only: {e}");
                None
            }
        };
        let handler = Handler::new(engine, window);

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
}

impl Handler {
    fn new(engine: Engine, window: Option<CandidateWindow>) -> Self {
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
        _server: &mut S,
        _user_ic: &mut UserInputContext<Self::InputContextData>,
    ) -> Result<(), ServerError> {
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
            let preedit = match outcome {
                Outcome::Consumed => Some(engine.preedit().to_owned()),
                _ => None,
            };
            (outcome, preedit)
        };

        match outcome {
            Outcome::Consumed => {
                if let Some(preedit) = preedit {
                    server.preedit_draw(&mut user_ic.ic, &preedit)?;
                }
                self.update_window();
                Ok(true)
            }
            Outcome::Commit(text) => {
                server.preedit_draw(&mut user_ic.ic, "")?;
                server.commit(&user_ic.ic, &text)?;
                self.hide_window();
                Ok(true)
            }
            Outcome::Ignored => Ok(false),
        }
    }
}
