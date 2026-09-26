//! X11 候选窗纯像素渲染：cosmic-text 横排候选 → ARGB8888 字节缓冲。
//!
//! 内存序：ARGB8888 小端 = B,G,R,A，可直接喂 XPutImage。
//! 本模块不碰 X11：无连接、无窗口、无绘图调用（那是 window.rs/T3 的事），
//! 布局与光栅都是纯计算，tests/candidate_render.rs 直接对返回值断言。
//!
//! 排版参数、配色、字形测量与光栅共用 `kime-render`（与 platform-wayland 同一份内核，
//! 同字号、同留白、同颜色）；本模块只留 X11 特有的东西：候选状态、窗口摆位坐标、
//! 整帧输出。

use kime_core::Candidate;
use kime_render::{fill_background, panel_height, panel_width, TextPainter, FG, HL};

pub use kime_render::{FONT_SIZE, LINE_HEIGHT, MARGIN_X, MARGIN_Y, SEP};

/// 一帧渲染结果：`pixels` 长度恒为 `width * height * 4`（ARGB8888 小端）。
/// 空候选/隐藏 → 1×1 全透明帧（真实候选条最小也有 2*边距+行高，绝不可能是 1×1）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RenderedFrame {
    fn hidden() -> Self {
        Self {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 0],
        }
    }

    /// 隐藏帧（空候选或不可见）：窗口不该被 PutImage 出去。
    pub fn is_hidden(&self) -> bool {
        self.width == 1 && self.height == 1
    }

    pub fn pixel_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// 纯像素 renderer：持有候选状态与字体系统，输出帧缓冲，不创建任何 X11 资源。
pub struct Renderer {
    candidates: Vec<Candidate>,
    highlight: usize,
    visible: bool,
    x: i32,
    y: i32,
    painter: TextPainter,
}
impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    /// 渲染器由 X11IM::new 在进事件循环前建好，`kime-render` 的字形冷路径预热
    /// 成本落在进程启动期而非 XIM 按键同步路径。
    pub fn new() -> Self {
        Self {
            candidates: Vec::new(),
            highlight: 0,
            visible: false,
            x: 0,
            y: 0,
            painter: TextPainter::new(),
        }
    }

    /// 候选列表更新（engine 新一轮查询结果）；高亮重置，非空即显示。
    pub fn set_candidates(&mut self, candidates: &[Candidate]) {
        self.candidates = candidates.to_vec();
        self.highlight = 0;
        self.visible = !self.candidates.is_empty();
    }

    /// 设置当前高亮项下标；越界只是不着色，render 不 panic。
    pub fn set_highlight(&mut self, highlight: usize) {
        self.highlight = highlight;
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }

    /// 窗口应放置的屏幕坐标（T3 用来移动候选窗）。
    pub fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn update_from_glyph_position(&mut self, x: i32, y: i32) {
        self.set_position(x + 5, y + 25);
    }

    /// 主入口：当前候选 + 高亮 → ARGB8888 帧。空候选/隐藏 → 1×1 透明帧。
    pub fn render(&mut self) -> RenderedFrame {
        if !self.visible || self.candidates.is_empty() {
            return RenderedFrame::hidden();
        }

        // 横排单行摆放：每项 "N. 候选"，定宽分隔，总宽随内容自适应。
        // 先把标签全部生成出来（measure 要 &mut self，与遍历借用冲突）
        let labels: Vec<String> = self
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}", i + 1, c.text))
            .collect();
        let mut items: Vec<(u32, String)> = Vec::with_capacity(labels.len());
        let mut x = MARGIN_X;
        for label in &labels {
            let w = self.painter.measure(label);
            items.push((x, label.clone()));
            x += w + SEP;
        }
        let width = panel_width(x);
        let height = panel_height();
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        fill_background(&mut pixels);
        for (idx, (item_x, label)) in items.iter().enumerate() {
            let color = if idx == self.highlight { HL } else { FG };
            self.painter
                .draw_run(label, color, &mut pixels, width as usize, *item_x as i32);
        }

        RenderedFrame {
            width,
            height,
            pixels,
        }
    }
}
