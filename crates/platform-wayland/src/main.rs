//! M3: input-method-v2 平台壳。简单 spike：grab keyboard，按啥 commit 啥。
//!
//! 运行：WAYLAND_DISPLAY=wayland-1 ./target/debug/platform-wayland [--seat <name>]
//! 退出：Esc 按下

use std::env;

use wayland_client::{
    globals::{registry_queue_init, GlobalList, GlobalListContents},
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

use kime_core::dict::Dict;
use kime_core::{config::Config, Engine, Key, Outcome};

fn log(msg: &str) {
    eprintln!("[zwp-spike] {}", msg);
}

struct AppState {
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method: Option<ZwpInputMethodV2>,
    engine: Option<Engine>,
    should_exit: bool,
    /// 协议要求：commit(serial) 的 serial = 已收到的 done 事件数
    im_serial: u32,
}

impl AppState {
    fn new() -> Self {
        Self {
            input_method_manager: None,
            input_method: None,
            engine: None,
            should_exit: false,
            im_serial: 0,
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
        _qh: &QueueHandle<Self>,
    ) {
        if let RegistryEvent::Global {
            name,
            interface,
            version,
        } = event
        {
            log(&format!("Global: {} v{} ({})", name, version, interface));
            if interface == "zwp_input_method_manager_v2" && version >= 1 {
                let mgr =
                    _registry.bind::<ZwpInputMethodManagerV2, (), AppState>(name, 1, _qh, ()) as _;
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
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for AppState {
    fn event(
        _state: &mut Self,
        _mgr: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for AppState {
    fn event(
        state: &mut Self,
        im: &ZwpInputMethodV2,
        event: ZwpInputMethodEvent,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodEvent::Activate => {
                log("input_method ACTIVATE");
                state.input_method = Some(im.clone());
                let config = Config::default();
                let dict_path = config.dict_path.clone();
                if let Ok(dict) = Dict::open(&dict_path) {
                    state.engine = Some(Engine::new(dict, config));
                    log("engine initialized");
                } else {
                    log("failed to initialize dictionary");
                }
                let _: ZwpInputMethodKeyboardGrabV2 = im.grab_keyboard(qh, ()) as _;
                log("grab_keyboard requested");
            }
            ZwpInputMethodEvent::Deactivate => {
                log("input_method DEACTIVATE");
                state.input_method = None;
                state.engine = None;
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
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
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

                let ch = match key {
                    1 => '\u{1b}',
                    30..=54 => (b'A' + (key - 30) as u8) as char,
                    11..=20 => (b'0' + (key - 11) as u8) as char,
                    57 => ' ',
                    28 => '\n',
                    _ => return,
                };

                if let Some(engine) = &mut state.engine {
                    let key_struct = Key {
                        ch: Some(ch),
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
                        }
                        Outcome::Commit(text) => {
                            log(&format!("engine commit: {}", text));
                            if let Some(im) = &state.input_method {
                                im.commit_string(text);
                                im.commit(state.im_serial);
                            }
                        }
                        Outcome::Ignored => {
                            log("engine ignored (passthrough)");
                        }
                    }
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
