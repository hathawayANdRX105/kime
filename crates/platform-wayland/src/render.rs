//! 候选窗绘制（input-popup 内容）：cosmic-text 横排单行 → Argb8888 字节缓冲。
//!
//! 内存序：ARGB8888 小端 = B,G,R,A。本模块不碰 wayland：`layout`/`paint`
//! 都是纯计算 + 光栅，tests/popup_render.rs 直接对返回值断言。
//!
//! 排版参数、配色、字形测量与光栅共用 `kime-render`（与 platform-x11 同一份内核）；
//! 本模块只留 Wayland 特有的东西：候选条摆位、页内高亮下标、模式字闪现小窗。

use cosmic_text::Color;
use kime_render::{fill_background, panel_height, panel_width, TextPainter, FG, HL, MAX_FRAME_DIM};

pub use kime_render::{FONT_SIZE, LINE_HEIGHT, MARGIN_X, MARGIN_Y, SEP};

/// 模式字：青绿系，与 FG/HL 及其抗锯齿混色都不撞
const CHIP: Color = Color::rgba(122, 207, 214, 255);

/// 模式标识字：中英切换瞬间 `chip_layout` 闪一次小窗用（常驻候选条不摆模式字）。
pub fn mode_chip(chinese: bool) -> &'static str {
    if chinese {
        "中"
    } else {
        "英"
    }
}

/// 模式 chip 闪现窗的终宽：左右留白 + 字形实测宽，钳在**终宽 ≤ MAX_FRAME_DIM**。
/// 钳位基准是 `create_buffer` 的守卫 `w > MAX_FRAME_DIM`（守卫看的是终宽）：若钳字宽到
/// 上限，终宽 = MARGIN_X*2 + 上限 = 越界。故字形宽先钳到 `上限 - MARGIN_X * 2`。
/// 正常单字 measure 仅 ~36px，钳位不触发；抽成纯函数只为钉住钳位基准（不依赖字体）。
pub fn chip_window_width(glyph_w: u32) -> u32 {
    MARGIN_X * 2 + glyph_w.min(MAX_FRAME_DIM - MARGIN_X * 2)
}

/// 单个已摆放的项：x 为内容左沿，w 为实测宽；chip = 模式字（只出现在 `chip_layout`
/// 闪现小窗，不编号、不可选）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedItem {
    pub x: u32,
    pub w: u32,
    pub text: String,
    pub chip: bool,
}

/// 一帧的完整摆放结果。空候选 → 1×1（隐藏帧，像素全透明）。
/// 候选条只摆 "N. 候选"；模式字不常驻，中英切换瞬间另走 `chip_layout`。
#[derive(Clone, Debug)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub items: Vec<PlacedItem>,
}

impl Layout {
    pub fn hidden() -> Self {
        Self {
            width: 1,
            height: 1,
            items: Vec::new(),
        }
    }
    pub fn is_hidden(&self) -> bool {
        self.items.is_empty()
    }
    pub fn pixel_len(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 4
    }
}

pub struct Renderer {
    painter: TextPainter,
}

impl Renderer {
    /// 预热好的渲染器（`kime-render` 的字形冷路径已 warmup），move 进来：
    /// 首次 ACTIVATE 不再付 ~110ms 的字体解析/fallback 成本
    pub fn new() -> Self {
        Self {
            painter: TextPainter::new(),
        }
    }

    /// 横排单行摆放：每项 "N. 候选"，定宽分隔，总宽随内容自适应。
    /// candidates 已是当前页（调用方切好片），这里不再截断。
    /// 模式字不常驻候选条：中英切换只闪一次 `chip_layout` 小窗。
    /// 空候选 → 隐藏帧。
    pub fn layout(&mut self, candidates: &[String]) -> Layout {
        if candidates.is_empty() {
            return Layout::hidden();
        }
        let mut items = Vec::with_capacity(candidates.len());
        let mut x = MARGIN_X;
        for (i, text) in candidates.iter().enumerate() {
            let label = format!("{}. {}", i + 1, text);
            let w = self.painter.measure(&label);
            items.push(PlacedItem {
                x,
                w,
                text: label,
                chip: false,
            });
            x += w + SEP;
        }
        let width = panel_width(x);
        Layout {
            width,
            height: panel_height(),
            items,
        }
    }

    /// 模式提示的一次性闪现小窗：只含 `中`/`英` 单字，下一次按键即清。
    /// 高度与候选条同律，宽度走 `chip_window_width`。
    pub fn chip_layout(&mut self, chinese: bool) -> Layout {
        let text = mode_chip(chinese).to_string();
        let measured = self.painter.measure(&text);
        let w = measured.min(MAX_FRAME_DIM - MARGIN_X * 2);
        Layout {
            width: chip_window_width(measured),
            height: panel_height(),
            items: vec![PlacedItem {
                x: MARGIN_X,
                w,
                text,
                chip: true,
            }],
        }
    }

    /// 把 layout 画进 `buf`（长度必须恰好 `layout.pixel_len()`，即 stride=w*4）。
    /// `highlight` 是页内下标；越界只是不着色，不 panic。
    pub fn paint(&mut self, layout: &Layout, highlight: usize, buf: &mut [u8]) {
        assert!(
            layout.width > 0 && layout.height > 0,
            "popup buffer 尺寸非法"
        );
        assert_eq!(buf.len(), layout.pixel_len(), "popup buffer 长度与布局不符");
        if layout.is_hidden() {
            buf.fill(0);
            return;
        }
        fill_background(buf);
        // highlight 是页内候选下标，chip 项（闪现小窗的唯一项）永远不算候选
        let mut cand_idx = 0usize;
        for item in &layout.items {
            let color = if item.chip {
                CHIP
            } else {
                let c = if cand_idx == highlight { HL } else { FG };
                cand_idx += 1;
                c
            };
            self.painter
                .draw_run(&item.text, color, buf, layout.width as usize, item.x as i32);
        }
    }
}
