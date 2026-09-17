//! XIM smoke client: connect to the `@server=kime` XIM server, create an IC,
//! synthesize key presses for "ni", observe the preedit, then commit and
//! verify the commit string. Run with the kime XIM server already running
//! (`kime-switch kime`); `DISPLAY` is inherited from the environment.
//!
//! Exit 0 = full path verified (open → IC → preedit → commit).
//! Exit 1 = any stage failed (message on stdout).

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt, CreateWindowAux, EventMask, KeyPressEvent, KEY_PRESS_EVENT,
};
use x11rb::protocol::Event;
use xim::{x11rb::X11rbClient, Client, ClientHandler};
use xim_parser::ForwardEventFlag;

const IM_LOCALE: &str = "en_US";
// X11 keycodes (US layout): n=39, i=25, space=65
const KEY_N: u8 = 39;
const KEY_I: u8 = 25;
const KEY_SPACE: u8 = 65;

struct SmokeHandler {
    im_id: u16,
    ic_id: u16,
    ic_created: bool,
    preedit_seen: String,
    commit_seen: Option<String>,
}

impl Default for SmokeHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl SmokeHandler {
    fn new() -> Self {
        Self {
            im_id: 0,
            ic_id: 0,
            ic_created: false,
            preedit_seen: String::new(),
            commit_seen: None,
        }
    }
}

impl<C: xim::Client<XEvent = KeyPressEvent>> ClientHandler<C> for SmokeHandler {
    fn handle_connect(&mut self, client: &mut C) -> Result<(), xim::ClientError> {
        client.open(IM_LOCALE)
    }

    fn handle_open(
        &mut self,
        client: &mut C,
        input_method_id: u16,
    ) -> Result<(), xim::ClientError> {
        self.im_id = input_method_id;
        println!("[smoke] XIM open ok, im_id={input_method_id}");
        client.get_im_values(
            input_method_id,
            &vec![xim_parser::AttributeName::QueryInputStyle],
        )
    }

    fn handle_get_im_values(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        _attributes: xim::AHashMap<xim_parser::AttributeName, Vec<u8>>,
    ) -> Result<(), xim::ClientError> {
        // 需要 client 建窗后才知道 client window；用 root=NONE 之外的方式：
        // 这里直接 create_ic，ClientWindow 用 0 占位（server 侧 spot translate 会退到 client_win）
        let _ = input_method_id;
        client.create_ic(
            self.im_id,
            client
                .build_ic_attributes()
                .push(
                    xim_parser::AttributeName::InputStyle,
                    xim_parser::InputStyle::PREEDIT_CALLBACKS,
                )
                .build(),
        )
    }

    fn handle_create_ic(
        &mut self,
        _client: &mut C,
        _im_id: u16,
        input_context_id: u16,
    ) -> Result<(), xim::ClientError> {
        self.ic_id = input_context_id;
        self.ic_created = true;
        println!("[smoke] IC created, ic_id={input_context_id}");
        Ok(())
    }

    fn handle_preedit_draw(
        &mut self,
        _client: &mut C,
        _im_id: u16,
        _ic_id: u16,
        _caret: i32,
        _chg_first: i32,
        _chg_len: i32,
        _status: xim_parser::PreeditDrawStatus,
        preedit_string: &str,
        _feedbacks: Vec<xim_parser::Feedback>,
    ) -> Result<(), xim::ClientError> {
        if !preedit_string.is_empty() {
            self.preedit_seen = preedit_string.to_string();
            println!("[smoke] preedit_draw: {preedit_string}");
        }
        Ok(())
    }

    fn handle_commit(
        &mut self,
        _client: &mut C,
        _im_id: u16,
        _ic_id: u16,
        text: &str,
    ) -> Result<(), xim::ClientError> {
        println!("[smoke] commit: {text}");
        self.commit_seen = Some(text.to_string());
        Ok(())
    }
}

fn key_press(root: u32, detail: u8) -> KeyPressEvent {
    KeyPressEvent {
        response_type: KEY_PRESS_EVENT,
        detail,
        sequence: 0,
        time: 0,
        root,
        event: root,
        child: 0,
        root_x: 0,
        root_y: 0,
        event_x: 0,
        event_y: 0,
        state: x11rb::protocol::xproto::KeyButMask::from(0u16),
        same_screen: true,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::rust_connection::RustConnection::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let window = conn.generate_id()?;
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        320,
        200,
        0,
        x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::default().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
    )?;
    conn.map_window(window)?;
    conn.flush()?;

    let mut client = X11rbClient::init(&conn, screen_num, Some("kime"))?;
    println!("[smoke] X11rbClient connected to @server=kime");

    let mut handler = SmokeHandler::new();

    // 1) 握手：connect → open → get_im_values → create_ic
    for _ in 0..30 {
        let event = conn.wait_for_event()?;
        client.filter_event(&event, &mut handler)?;
        if handler.ic_created {
            break;
        }
        if matches!(event, Event::Error(_)) {
            eprintln!("[smoke] X error during handshake");
            std::process::exit(1);
        }
    }
    if !handler.ic_created {
        println!("[smoke] FAIL: IC not created");
        std::process::exit(1);
    }

    // 2) 输入 "n"、"i"
    client.forward_event(
        handler.im_id,
        handler.ic_id,
        ForwardEventFlag::empty(),
        &key_press(window, KEY_N),
    )?;
    client.forward_event(
        handler.im_id,
        handler.ic_id,
        ForwardEventFlag::empty(),
        &key_press(window, KEY_I),
    )?;

    for _ in 0..20 {
        let event = conn.wait_for_event()?;
        let filtered = client.filter_event(&event, &mut handler)?;
        if !handler.preedit_seen.is_empty() {
            break;
        }
        if filtered {
            continue;
        }
        if matches!(event, Event::Error(_)) {
            break;
        }
    }

    if handler.preedit_seen.is_empty() {
        println!("[smoke] FAIL: no preedit after 'ni'");
        std::process::exit(1);
    }
    println!("[smoke] preedit observed: {:?}", handler.preedit_seen);

    // 3) 空格上屏首选（kime 引擎：有候选时空格 commit 首选）
    client.forward_event(
        handler.im_id,
        handler.ic_id,
        ForwardEventFlag::empty(),
        &key_press(window, KEY_SPACE),
    )?;
    for _ in 0..20 {
        let event = conn.wait_for_event()?;
        let filtered = client.filter_event(&event, &mut handler)?;
        if handler.commit_seen.is_some() {
            break;
        }
        if filtered {
            continue;
        }
        if matches!(event, Event::Error(_)) {
            break;
        }
    }

    match &handler.commit_seen {
        Some(text) if !text.is_empty() => {
            println!("[smoke] PASS: commit text = {text}");
            Ok(())
        }
        Some(_) => {
            println!("[smoke] FAIL: empty commit text");
            std::process::exit(1);
        }
        None => {
            println!("[smoke] FAIL: no commit after space");
            std::process::exit(1);
        }
    }
}
