//! M3: input-method-v2 平台壳。简单 spike：grab keyboard，按啥 commit 啥。
//!
//! 运行：WAYLAND_DISPLAY=wayland-1 ./target/debug/platform-wayland [--seat <name>]
//! 退出：Esc 按下

use std::env;
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

fn log(msg: &str) {
    eprintln!("[zwp-spike] {}", msg);
}

/// LLM 异步工作线程
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

struct AppState {
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    engine: Option<Engine>,
    window: Option<CandidateWindow>,
    should_exit: bool,
    im_serial: u32,
    tray: TrayIconManager,
    llm_worker: Option<LlmWorker>,
    llm_receiver: Option<Receiver<Vec<kime_core::dict::Candidate>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            input_method_manager: None,
            input_method: None,
            engine: None,
            window: None,
            should_exit: false,
            im_serial: 0,
            tray: TrayIconManager::new(true),
            llm_worker: None,
            llm_receiver: None,
        }
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
}

impl Dispatch<WlRegistry, GlobalListContents> for AppState {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
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
            log(&format!("Global: {} v{} ({})", name, version, interface));
            if interface == "zwp_input_method_manager_v2" && version >= 1 {
                let mgr = _registry
                    .bind::<ZwpInputMethodManagerV2, (), AppState>(name, 1, qh, ())
                    as _;
                state.input_method_manager = Some(mgr);
                log(&format!("bound input method manager name={}", name));
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

impl Dispatch<ZwpInputMethodV2, ()> for AppState {
    fn event(
        state: &mut Self,
        im: &ZwpInputMethodV2,
        event: ZwpInputMethodEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodEvent::Activate => {
                log("input_method ACTIVATE");
                state.input_method = Some(im.clone());
                let config = Config::load();
                let dict_path = config.dict_path.clone();
                if let Ok(dict) = Dict::open(&dict_path) {
                    let engine = Engine::new(dict, config.clone());
                    state.engine = Some(engine);
                    log("engine initialized");
                    let (tx, rx) = channel();
                    let endpoint = config.ai_endpoint.clone().unwrap_or_default();
                    let model = config.ai_model.clone();
                    state.llm_worker = Some(LlmWorker::new(endpoint, model, tx));
                    state.llm_receiver = Some(rx);
                } else {
                    log("failed to initialize dictionary");
                }
                let _: ZwpInputMethodKeyboardGrabV2 = im.grab_keyboard(_qh, ()) as _;
                log("grab_keyboard requested");
            }
            ZwpInputMethodEvent::Deactivate => {
                log("input_method DEACTIVATE");
                state.input_method = None;
                state.engine = None;
                state.llm_worker = None;
                state.llm_receiver = None;
            }
            ZwpInputMethodEvent::Done { .. } => {
                state.im_serial += 1;
            }
            ZwpInputMethodEvent::Unavailable => {
                log("input_method UNAVAILABLE — exiting");
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
            ZwpInputMethodKeyboardGrabEvent::Key {
                key,
                state: key_state,
                ..
            } => {
                if !matches!(key_state, WEnum::Value(KeyState::Pressed)) {
                    return;
                }

                let is_page_key = matches!(key, 12 | 13 | 26 | 27);
                let ch = match key {
                    1 => Some('\u{1b}'),
                    30..=54 => Some((b'A' + (key - 30) as u8) as char),
                    11..=20 => Some((b'0' + (key - 11) as u8) as char),
                    57 => Some(' '),
                    28 => Some('\n'),
                    _ if is_page_key => None,
                    _ => return,
                };

                if let Some(engine) = &mut state.engine {
                    let key_struct = Key {
                        ch,
                        code: key,
                        shift: false,
                        ctrl: false,
                        alt: false,
                    };
                    match engine.key(key_struct) {
                        Outcome::Consumed => {
                            let pe = engine.preedit().to_string();
                            log(&format!("engine consumed: preedit={}", pe));
                            if let Some(im) = &state.input_method {
                                let cursor = pe.len() as i32;
                                im.set_preedit_string(pe, 0, cursor);
                                im.commit(state.im_serial);
                            }
                            if let Some(win) = &mut state.window {
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
                                    let _ = win.hide();
                                } else {
                                    let _ =
                                        win.show(&ui, engine.highlight(), engine.preedit());
                                }
                            }
                            if let Some(worker) = &state.llm_worker {
                                worker.request(engine.preedit().split("'").map(String::from).collect());
                            }
                        }
                        Outcome::Commit(text) => {
                            log(&format!("engine commit: {}", text));
                            if let Some(im) = &state.input_method {
                                im.commit_string(text);
                                im.commit(state.im_serial);
                            }
                            if let Some(win) = &mut state.window {
                                let _ = win.hide();
                            }
                        }
                        Outcome::Ignored => {
                            // 引擎未消费此键，原样提交给应用（英文模式/数字直接上屏）
                            if let Some(c) = ch {
                                if let Some(im) = &state.input_method {
                                    let _ = im.commit_string(c.to_string());
                                    im.commit(state.im_serial);
                                }
                            }
                            log(&format!("engine ignored: ch={:?} passthrough", ch));
                        }
                    }
                    state.try_recv_llm();
                }
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

    if let Some(ref s) = target_seat {
        log(&format!("目标 seat: {}", s));
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
            UiCandidate {
                text: "泥号".into(),
                pinyin: "ni'hao".into(),
                freq: 10,
                ai: false,
            },
        ];
        win.show(&ui, 0, "nihao")?;
        std::thread::sleep(std::time::Duration::from_secs(9));
        win.hide()?;
        return Ok(());
    }

    log("开始事件循环...");
    let (globals, mut event_queue) = registry_queue_init::<AppState>(&conn)?;
    let qh: QueueHandle<AppState> = event_queue.handle();

    let mut app = AppState::new();

    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == "zwp_input_method_manager_v2" && global.version >= 1 {
                log(&format!(
                    "Found initial global: {} v{} ({})",
                    global.name, global.version, global.interface
                ));
                let mgr = globals
                    .registry()
                    .bind::<ZwpInputMethodManagerV2, (), AppState>(global.name, 1, &qh, ())
                    as _;
                app.input_method_manager = Some(mgr);
                log("bound input method manager from initial burst");
            }
        }
    });

    log("开始事件循环...");

    while !app.should_exit {
        event_queue.blocking_dispatch(&mut app)?;
    }

    log("正常退出");
    Ok(())
}
