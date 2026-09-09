//! X11 候选窗渲染模块
//!
//! 实现基于 X11 的候选词窗口绘制

use kime_core::Candidate;

pub struct Renderer {
    candidates: Vec<Candidate>,
    current_page: usize,
    page_size: usize,
    x: i32,
    y: i32,
    visible: bool,
    width: u16,
    height: u16,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            candidates: Vec::new(),
            current_page: 0,
            page_size: 10,
            x: 0,
            y: 0,
            visible: false,
            width: 400,
            height: 300,
        }
    }

    pub fn set_candidates(&mut self, candidates: &[Candidate]) {
        self.candidates = candidates.to_vec();
        self.current_page = 0;
        self.visible = !self.candidates.is_empty();
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn render(&self) {
        if !self.visible || self.candidates.is_empty() {
            return;
        }

        let start = self.current_page * self.page_size;
        let end = (start + self.page_size).min(self.candidates.len());
        let page = &self.candidates[start..end];

        println!(
            "[X11 Render] Candidate window at ({}, {}), {} candidates:",
            self.x,
            self.y,
            page.len()
        );
        for (i, candidate) in page.iter().enumerate() {
            if i == 0 {
                println!(
                    "[X11 Render] * Highlighted: {}. {}",
                    start + i + 1,
                    candidate.text
                );
            } else {
                println!("[X11 Render]   {}. {}", start + i + 1, candidate.text);
            }
        }
    }

    pub fn update_from_glyph_position(&mut self, x: i32, y: i32) {
        let candidate_x = x + 5;
        let candidate_y = y + 25;
        self.set_position(candidate_x, candidate_y);
    }
}
