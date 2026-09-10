//! input-method-v2 平台壳：绑 seat → get_input_method → grab → engine。
//! 未消费的键经 zwp_virtual_keyboard_v1 原样送回，避免 grab 吞掉回车/快捷键。

use std::collections::HashSet;
use std::env;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc::{channel, Receiver, Sender};

use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::llm::{Debouncer, LlmClient};
use kime_core::{Engine, Key, Outcome};
use platform_wayland::tray::TrayIconManager;
use platform_wayland::window::CandidateWindow;
use platform_wayland::Candidate as UiCandidate;
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_keyboard::KeyState,
        wl_registry::{Event as RegistryEvent, WlRegistry},
        wl_seat::{self, WlSeat},
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{
        Event as ZwpInputMethodKeyboardGrabEvent, ZwpInputMethodKeyboardGrabV2,
    },
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{Event as ZwpInputMethodEvent, ZwpInputMethodV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

fn log(msg: &str) {
    eprintln!("[kime-ime] {}", msg);
}

fn dup_fd(fd: impl AsFd) -> Option<OwnedFd> {
    let raw = unsafe { libc::dup(fd.as_fd().as_raw_fd()) };
    if raw < 0 {
        None
    } else {
        Some(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

/// US QWERTY evdev KEY_* → 字符。功能键 ch=None，靠 code 走 engine。
fn evdev_char(code: u32) -> Option<char> {
    match code {
        16 => Some('q'),
        17 => Some('w'),
        18 => Some('e'),
        19 => Some('r'),
        20 => Some('t'),
        21 => Some('y'),
        22 => Some('u'),
        23 => Some('i'),
        24 => Some('o'),
        25 => Some('p'),
        30 => Some('a'),
        31 => Some('s'),
        32 => Some('d'),
        33 => Some('f'),
        34 => Some('g'),
        35 => Some('h'),
        36 => Some('j'),
        37 => Some('k'),
        38 => Some('l'),
        44 => Some('z'),
        45 => Some('x'),
        46 => Some('c'),
        47 => Some('v'),
        48 => Some('b'),
        49 => Some('n'),
        50 => Some('m'),
        2 => Some('1'),
        3 => Some('2'),
        4 => Some('3'),
        5 => Some('4'),
        6 => Some('5'),
        7 => Some('6'),
        8 => Some('7'),
        9 => Some('8'),
        10 => Some('9'),
        11 => Some('0'),
        12 => Some('-'),
        13 => Some('='),
        26 => Some('['),
        27 => Some(']'),
        52 => Some('.'),
        _ => None,
    }
}

struct LlmWorker {
    runtime: tokio::runtime::Runtime,
    client: LlmClient,
    debouncer: Debouncer,
    result_sender: Sender<Vec<kime_core::dict::Candidate>>,
}

impl LlmWorker {
    fn new(
        endpoint: String,
        model: String,
        result_sender: Sender<Vec<kime_core::dict::Candidate>>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime");
        let client = LlmClient::new(endpoint, model);
        let debouncer = Debouncer::new(std::time::Duration::from_millis(200));
        Self {
            runtime,
            client,
            debouncer,
            result_sender,
        }
    }

    fn request(&self, syllables: Vec<String>) {
        let client = self.client.clone();
        let debouncer = self.debouncer.clone();
        let sender = self.result_sender.clone();
        self.runtime.spawn(async move {
            if debouncer.should_fire().await {
                match client.request_async(syllables).await {
                    Ok(candidates) => {
                        let _ = sender.send(candidates);
                    }
                    Err(e) => log(&format!("LLM error: {}", e)),
                }
            }
        });
    }
}

struct SeatBind {
    seat: WlSeat,
    name: Option<String>,
}

struct AppState {
    conn: Option<Connection>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    grab: Option<ZwpInputMethodKeyboardGrabV2>,
    vk_manager: Option<ZwpVirtualKeyboardManagerV1>,
    vk: Option<ZwpVirtualKeyboardV1>,
    vk_keymap_ready: bool,
    seats: Vec<SeatBind>,
    target_seat: Option<String>,
    engine: Option<Engine>,
    window: Option<CandidateWindow>,
    should_exit: bool,
    im_serial: u32,
    tray: TrayIconManager,
    llm_worker: Option<LlmWorker>,
    llm_receiver: Option<Receiver<Vec<kime_core::dict::Candidate>>>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    /// grab 吃掉 press 的键，release 也不转发，避免半截按键
    swallowed: HashSet<u32>,
}

impl AppState {
    fn new(target_seat: Option<String>) -> Self {
        Self {
            conn: None,
            input_method_manager: None,
            input_method: None,
            grab: None,
            vk_manager: None,
            vk: None,
            vk_keymap_ready: false,
            seats: Vec::new(),
            target_seat,
            engine: None,
            window: None,
            should_exit: false,
            im_serial: 0,
            tray: TrayIconManager::new(true),
            llm_worker: None,
            llm_receiver: None,
            ctrl: false,
            alt: false,
            shift: false,
            swallowed: HashSet::new(),
        }
    }

    fn pick_seat(&self) -> Option<&SeatBind> {
        if let Some(want) = &self.target_seat {
            self.seats
                .iter()
                .find(|s| s.name.as_deref() == Some(want.as_str()))
                .or_else(|| self.seats.first())
        } else {
            self.seats.first()
        }
    }

    fn try_bind_im(&mut self, qh: &QueueHandle<Self>) {
        if self.input_method.is_some() {
            return;
        }
        let Some(mgr) = self.input_method_manager.as_ref() else {
            return;
        };
        let Some(seat) = self.pick_seat() else {
            return;
        };
        let im = mgr.get_input_method(&seat.seat, qh, ());
        log("get_input_method issued");
        self.input_method = Some(im);
    }

    fn try_bind_vk(&mut self, qh: &QueueHandle<Self>) {
        if self.vk.is_some() {
            return;
        }
        let Some(mgr) = self.vk_manager.as_ref() else {
            return;
        };
        let Some(seat) = self.pick_seat() else {
            return;
        };
        let vk = mgr.create_virtual_keyboard(&seat.seat, qh, ());
        log("virtual keyboard created");
        self.vk = Some(vk);
    }

    fn forward_key(&self, time: u32, key: u32, pressed: bool) {
        let Some(vk) = &self.vk else {
            log("vk missing, cannot forward key");
            return;
        };
        if !self.vk_keymap_ready {
            log("vk keymap not ready, drop forward");
            return;
        }
        let state = if pressed { 1 } else { 0 };
        vk.key(time, key, state);
    }

    fn try_recv_llm(&mut self) {
        if let Some(receiver) = &self.llm_receiver {
            if let Ok(candidates) = receiver.try_recv() {
                if let Some(engine) = &mut self.engine {
                    engine.merge_ai(candidates);
                }
            }
        }
    }

    fn ensure_engine(&mut self) {
        if self.engine.is_some() {
            return;
        }
        let config = Config::load();
        let dict_path = config.dict_path.clone();
        match Dict::open(&dict_path) {
            Ok(dict) => {
                log(&format!("dict opened: {dict_path}"));
                let engine = Engine::new(dict, config.clone());
                self.tray.set_chinese(engine.chinese());
                self.engine = Some(engine);
                let (tx, rx) = channel();
                let endpoint = config.ai_endpoint.clone().unwrap_or_default();
                let model = config.ai_model.clone();
                self.llm_worker = Some(LlmWorker::new(endpoint, model, tx));
                self.llm_receiver = Some(rx);
            }
            Err(e) => log(&format!("failed to open dict {dict_path}: {e}")),
        }
    }

    fn ensure_window(&mut self, _im: &ZwpInputMethodV2) {
        if self.window.is_some() {
            return;
        }
        match CandidateWindow::new_layer() {
            Ok(win) => {
                self.window = Some(win);
                log("layer candidate window ready");
            }
            Err(e) => log(&format!("candidate window failed: {e}")),
        }
    }

    fn apply_consumed(&mut self) {
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        self.tray.set_chinese(engine.chinese());
        let pe = engine.preedit().to_string();
        log(&format!("consumed preedit={pe}"));
        if let Some(im) = &self.input_method {
            let cursor = pe.len() as i32;
            im.set_preedit_string(pe, 0, cursor);
            im.commit(self.im_serial);
        }
        if let Some(win) = &mut self.window {
            let ui: Vec<UiCandidate> = engine
                .candidates()
                .iter()
                .map(|c| UiCandidate {
                    text: c.text.clone(),
                    pinyin: c.pinyin.clone(),
                    freq: c.freq,
                    ai: c.ai,
                })
                .collect();
            if ui.is_empty() {
                if let Err(e) = win.hide() {
                    log(&format!("hide failed: {e}"));
                }
            } else if let Err(e) = win.show(&ui, engine.highlight(), engine.preedit()) {
                log(&format!("show failed: {e}"));
            }
        }
        if let Some(worker) = &self.llm_worker {
            if let Some(engine) = self.engine.as_ref() {
                worker.request(engine.preedit().split('\'').map(String::from).collect());
            }
        }
    }

    fn apply_commit(&mut self, text: String) {
        log(&format!("commit {text}"));
        if let Some(im) = &self.input_method {
            im.commit_string(text);
            im.set_preedit_string(String::new(), 0, 0);
            im.commit(self.im_serial);
        }
        if let Some(win) = &mut self.window {
            let _ = win.hide();
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for AppState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: RegistryEvent,
        _globals: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let RegistryEvent::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_input_method_manager_v2" if version >= 1 => {
                    let mgr =
                        registry.bind::<ZwpInputMethodManagerV2, (), AppState>(name, 1, qh, ());
                    state.input_method_manager = Some(mgr);
                    log(&format!("bound input method manager name={name}"));
                    state.try_bind_im(qh);
                }
                "zwp_virtual_keyboard_manager_v1" if version >= 1 => {
                    let mgr =
                        registry.bind::<ZwpVirtualKeyboardManagerV1, (), AppState>(name, 1, qh, ());
                    state.vk_manager = Some(mgr);
                    log("bound virtual keyboard manager");
                    state.try_bind_vk(qh);
                }
                "wl_seat" if version >= 1 => {
                    let ver = version.min(7);
                    let seat = registry.bind::<WlSeat, (), AppState>(name, ver, qh, ());
                    log(&format!("bound wl_seat name={name} v{ver}"));
                    state.seats.push(SeatBind { seat, name: None });
                    state.try_bind_im(qh);
                    state.try_bind_vk(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlRegistry, ()> for AppState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            log(&format!("seat name={name}"));
            if let Some(bind) = state.seats.iter_mut().find(|s| s.seat == *seat) {
                bind.name = Some(name);
            }
            state.try_bind_im(qh);
            state.try_bind_vk(qh);
        }
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for AppState {
    fn event(
        _state: &mut Self,
        _mgr: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _mgr: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _vk: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for AppState {
    fn event(
        state: &mut Self,
        im: &ZwpInputMethodV2,
        event: ZwpInputMethodEvent,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodEvent::Activate => {
                log("input_method ACTIVATE");
                state.input_method = Some(im.clone());
                state.ensure_engine();
                state.ensure_window(im);
                let grab = im.grab_keyboard(qh, ());
                log("grab_keyboard requested");
                state.grab = Some(grab);
                state.swallowed.clear();
            }
            ZwpInputMethodEvent::Deactivate => {
                log("input_method DEACTIVATE");
                state.grab = None;
                state.swallowed.clear();
                if let Some(win) = &mut state.window {
                    let _ = win.hide();
                }
            }
            ZwpInputMethodEvent::Done { .. } => {
                state.im_serial += 1;
            }
            ZwpInputMethodEvent::Unavailable => {
                log("input_method UNAVAILABLE — another IM owns the seat");
                state.should_exit = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for AppState {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: ZwpInputMethodKeyboardGrabEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodKeyboardGrabEvent::Keymap { format, fd, size } => {
                log(&format!("grab keymap size={size}"));
                if let Some(vk) = &state.vk {
                    if let Some(dup) = dup_fd(&fd) {
                        let fmt = match format {
                            WEnum::Value(v) => v as u32,
                            WEnum::Unknown(v) => v,
                        };
                        vk.keymap(fmt, dup.as_fd(), size);
                        state.vk_keymap_ready = true;
                        log("vk keymap set");
                    } else {
                        log("dup keymap fd failed");
                    }
                } else {
                    log("vk missing when keymap arrived");
                }
            }
            ZwpInputMethodKeyboardGrabEvent::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(vk) = &state.vk {
                    if state.vk_keymap_ready {
                        vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                    }
                }
            }
            ZwpInputMethodKeyboardGrabEvent::Key {
                serial: _,
                time,
                key,
                state: key_state,
            } => {
                let pressed = matches!(key_state, WEnum::Value(KeyState::Pressed));
                let released = matches!(key_state, WEnum::Value(KeyState::Released));

                match key {
                    29 | 97 => state.ctrl = pressed,
                    56 | 100 => state.alt = pressed,
                    42 | 54 => state.shift = pressed,
                    _ => {}
                }

                if released {
                    if state.swallowed.remove(&key) {
                        return;
                    }
                    state.forward_key(time, key, false);
                    return;
                }
                if !pressed {
                    return;
                }

                let is_shift = matches!(key, 42 | 54);
                let ch = if matches!(key, 1 | 14 | 28 | 42 | 54 | 57) {
                    None
                } else {
                    evdev_char(key)
                };

                let Some(engine) = state.engine.as_mut() else {
                    state.forward_key(time, key, true);
                    return;
                };
                let key_struct = Key {
                    ch,
                    code: key,
                    shift: is_shift,
                    ctrl: state.ctrl,
                    alt: state.alt,
                };
                match engine.key(key_struct) {
                    Outcome::Consumed => {
                        state.swallowed.insert(key);
                        state.apply_consumed();
                    }
                    Outcome::Commit(text) => {
                        state.swallowed.insert(key);
                        state.apply_commit(text);
                    }
                    Outcome::Ignored => {
                        state.forward_key(time, key, true);
                    }
                }
                state.try_recv_llm();
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut target_seat = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--seat" && i + 1 < args.len() {
            target_seat = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    if let Some(s) = &target_seat {
        log(&format!("目标 seat: {s}"));
    } else {
        log("使用默认 seat");
    }

    let conn = Connection::connect_to_env()?;
    if args.iter().any(|a| a == "--show-test") {
        let mut win = CandidateWindow::new_layer()?;
        let ui = vec![
            UiCandidate {
                text: "你好".into(),
                pinyin: "ni'hao".into(),
                freq: 99,
                ai: false,
            },
            UiCandidate {
                text: "拟好".into(),
                pinyin: "ni'hao".into(),
                freq: 50,
                ai: false,
            },
        ];
        win.show(&ui, 0, "nihao")?;
        std::thread::sleep(std::time::Duration::from_secs(9));
        win.hide()?;
        return Ok(());
    }

    let (globals, mut event_queue) = registry_queue_init::<AppState>(&conn)?;
    let qh: QueueHandle<AppState> = event_queue.handle();
    let mut app = AppState::new(target_seat);
    app.conn = Some(conn);

    globals.contents().with_list(|list| {
        for global in list {
            match global.interface.as_str() {
                "zwp_input_method_manager_v2" if global.version >= 1 => {
                    log(&format!(
                        "Found IM manager: {} v{}",
                        global.name, global.version
                    ));
                    let mgr = globals
                        .registry()
                        .bind::<ZwpInputMethodManagerV2, (), AppState>(global.name, 1, &qh, ());
                    app.input_method_manager = Some(mgr);
                }
                "zwp_virtual_keyboard_manager_v1" if global.version >= 1 => {
                    log("Found virtual keyboard manager");
                    let mgr = globals
                        .registry()
                        .bind::<ZwpVirtualKeyboardManagerV1, (), AppState>(global.name, 1, &qh, ());
                    app.vk_manager = Some(mgr);
                }
                "wl_seat" if global.version >= 1 => {
                    let ver = global.version.min(7);
                    log(&format!("Found seat: {} v{ver}", global.name));
                    let seat =
                        globals
                            .registry()
                            .bind::<WlSeat, (), AppState>(global.name, ver, &qh, ());
                    app.seats.push(SeatBind { seat, name: None });
                }
                _ => {}
            }
        }
    });
    app.try_bind_im(&qh);
    app.try_bind_vk(&qh);
    log("event loop");

    while !app.should_exit {
        event_queue.blocking_dispatch(&mut app)?;
    }
    log("exit");
    Ok(())
}
