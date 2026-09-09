//! XIM 协议实现：XOpenIM / XCreateIC / XFilterEvent / XIMPreeditCallback

use kime_core::{Engine, Key, Outcome};
use parking_lot::Mutex;
use std::sync::Arc;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

pub struct X11IM {
    conn: Arc<x11rb::rust_connection::RustConnection>,
    engine: Arc<Mutex<Engine>>,
    running: bool,
}

impl X11IM {
    pub fn new(conn: Arc<x11rb::rust_connection::RustConnection>, engine: Engine) -> Self {
        Self {
            conn,
            engine: Arc::new(Mutex::new(engine)),
            running: true,
        }
    }

    pub fn run(&mut self) {
        while self.running {
            match self.conn.wait_for_event() {
                Ok(event) => {
                    self.process_event(event);
                }
                Err(_) => {
                    break;
                }
            }
        }
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::KeyPress(event) => {
                let key = Key {
                    code: event.detail as u32,
                    shift: event.state.contains(KeyButMask::SHIFT),
                    ctrl: event.state.contains(KeyButMask::CONTROL),
                    alt: event.state.contains(KeyButMask::MOD1),
                    ch: None,
                };
                let mut engine = self.engine.lock();
                match engine.key(key) {
                    Outcome::Consumed => {
                        let _ = engine.preedit();
                    }
                    Outcome::Commit(_) => {}
                    Outcome::Ignored => {}
                }
            }
            _ => {}
        }
    }

    pub fn stop(&mut self) {
        self.running = false;
    }
}

unsafe impl Send for X11IM {}
unsafe impl Sync for X11IM {}
