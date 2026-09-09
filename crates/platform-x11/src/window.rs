//! X11 候选窗管理
//!
//! 管理候选窗的创建、显示、隐藏及光标跟随定位
//! 使用 XIMGlyphPosition 实现 XWayland 应用的光标跟随

use crate::render::Renderer;
use kime_core::{Engine, Key, Outcome};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct CandidateWindow {
    renderer: Renderer,
    engine: Arc<Mutex<Engine>>,
    last_cursor_pos: (i32, i32),
}

impl CandidateWindow {
    pub fn new(engine: Arc<Mutex<Engine>>) -> Self {
        let renderer = Renderer::new();

        Self {
            renderer,
            engine,
            last_cursor_pos: (0, 0),
        }
    }

    pub fn handle_key(&mut self, key: Key) -> Outcome {
        let mut engine = self.engine.lock();
        let outcome = engine.key(key);

        match outcome {
            Outcome::Consumed => {
                let candidates = engine.candidates();
                if !candidates.is_empty() {
                    self.renderer.set_candidates(candidates);
                    self.renderer.set_visible(true);
                    self.renderer.render();
                }
            }
            Outcome::Commit(_) => {
                self.renderer.set_visible(false);
            }
            Outcome::Ignored => {}
        }

        outcome
    }

    pub fn update_cursor(&mut self, x: i32, y: i32) {
        self.last_cursor_pos = (x, y);
        self.renderer.update_from_glyph_position(x, y);
    }

    pub fn show(&mut self) {
        self.renderer.set_visible(true);
        self.renderer.render();
    }

    pub fn hide(&mut self) {
        self.renderer.set_visible(false);
    }

    pub fn run(&mut self) {}

    pub fn engine(&self) -> Arc<Mutex<Engine>> {
        self.engine.clone()
    }
}
