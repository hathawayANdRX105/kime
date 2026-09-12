//! 候选窗绘制（input-popup 内容）：cosmic-text 横排单行 → Argb8888 字节缓冲。
//!
//! 内存序：ARGB8888 小端 = B,G,R,A。本模块不碰 wayland：`layout`/`paint`
//! 都是纯计算 + 光栅，tests/popup_render.rs 直接对返回值断言。

use std::sync::atomic::{AtomicBool, Ordering};

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};

pub const FONT_SIZE: f32 = 16.0;
pub const LINE_HEIGHT: f32 = 22.0;
/// 面板左右留白
pub const MARGIN_X: u32 = 10;
/// 上下留白
pub const MARGIN_Y: u32 = 6;
/// 候选项之间的定宽分隔（px）
pub const SEP: u32 = 10;

/// 背景 (30,30,38)，不透明。B,G,R,A 序。
const BG: [u8; 4] = [38, 30, 30, 255];
const FG: Color = Color::rgba(220, 220, 230, 255);
/// 高亮项：旧面板同款琥珀色，深底对比足够
const HL: Color = Color::rgba(255, 220, 120, 255);
/// 模式字：青绿系，与 FG/HL 及其抗锯齿混色都不撞
const CHIP: Color = Color::rgba(122, 207, 214, 255);

/// 模式标识字：中文 = 候选条开头的 chip；英文 = 无候选时唯一的提示内容。
pub fn mode_chip(chinese: bool) -> &'static str {
    if chinese {
        "中"
    } else {
        "英"
    }
}

/// 单个已摆放的项：x 为内容左沿，w 为实测宽；chip = 模式字（不编号、不可选）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedItem {
    pub x: u32,
    pub w: u32,
    pub text: String,
    pub chip: bool,
}

/// 一帧的完整摆放结果。中文 + 空候选 → 1×1（隐藏帧，像素全透明）；
/// 英文 + 空候选 → 只含 `英` 字的提示小窗。
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

/// 系统里连一个能用的字形都量不到：明确吼一声，别默默画空白帧。
fn warn_no_font(text: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("[kime-popup] cosmic-text 量不到字形宽度（无可用字体？）：「{text}」");
    }
}

pub struct Renderer {
    font_system: FontSystem,
    cache: SwashCache,
}

impl Renderer {
    /// FontSystem::new() 扫系统字体（fontconfig 路径），一次性、偏慢，只建一个。
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    /// 测量一段文本的像素宽（横排、不换行）。
    /// 宽度取所有 run 的 `line_w` 最大值：script/BiDi 分段后各段不重叠，
    /// 单段宽度不是整行总宽。
    pub fn measure(&mut self, text: &str) -> u32 {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buffer.set_size(None, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let w = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0f32, f32::max);
        if w < 0.5 && !text.is_empty() {
            warn_no_font(text);
        }
        w.ceil().max(0.0) as u32
    }

    /// 横排单行摆放：模式字打头（不编号），其后每项 "N. 候选"，定宽分隔，
    /// 总宽随内容自适应。candidates 已是当前页（调用方切好片），这里不再截断。
    /// 高度只含边距 + 行高：合成器已把 popup 摆在光标旁，表面内不再留光标行空行。
    pub fn layout(&mut self, candidates: &[String], chinese: bool) -> Layout {
        if candidates.is_empty() && chinese {
            return Layout::hidden();
        }
        let mut items = Vec::with_capacity(candidates.len() + 1);
        let mut x = MARGIN_X;
        let chip = mode_chip(chinese);
        let w = self.measure(chip);
        items.push(PlacedItem {
            x,
            w,
            text: chip.to_string(),
            chip: true,
        });
        x += w + SEP;
        for (i, text) in candidates.iter().enumerate() {
            let label = format!("{}. {}", i + 1, text);
            let w = self.measure(&label);
            items.push(PlacedItem {
                x,
                w,
                text: label,
                chip: false,
            });
            x += w + SEP;
        }
        let width = (x - SEP + MARGIN_X).max(1);
        let height = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
        Layout {
            width,
            height,
            items,
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
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&BG);
        }
        let band_top = MARGIN_Y;
        let band_h = LINE_HEIGHT.ceil() as i32;
        // highlight 是页内候选下标，模式字永远不算候选
        let mut cand_idx = 0usize;
        for item in &layout.items {
            let color = if item.chip {
                CHIP
            } else {
                let c = if cand_idx == highlight { HL } else { FG };
                cand_idx += 1;
                c
            };
            let mut buffer =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buffer.set_size(None, None);
            buffer.set_text(&item.text, &Attrs::new(), Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
            // 单行：把 run 的行盒垂直居中到文本带
            let dy = match buffer.layout_runs().next() {
                Some(run) => {
                    band_top as i32 + (band_h - run.line_height as i32) / 2 - run.line_top as i32
                }
                None => band_top as i32,
            };
            let ox = item.x as i32;
            buffer.draw(
                &mut self.font_system,
                &mut self.cache,
                color,
                |x, y, w, h, c| {
                    composite_rect(buf, layout.width as usize, (x + ox, y + dy), (w, h), c);
                },
            );
        }
    }
}

/// 直行 alpha 合成到不透明底上（cosmic 回调色：rgb=字色，a=覆盖率）。
fn composite_rect(
    buf: &mut [u8],
    stride_px: usize,
    pos: (i32, i32),
    size: (u32, u32),
    color: Color,
) {
    let a = color.a() as u32;
    if a == 0 {
        return;
    }
    let (r, g, b) = (color.r() as u32, color.g() as u32, color.b() as u32);
    for yy in 0..size.1 as i32 {
        let py = pos.1 + yy;
        if py < 0 {
            continue;
        }
        for xx in 0..size.0 as i32 {
            let px = pos.0 + xx;
            if px < 0 {
                continue;
            }
            let off = (py as usize * stride_px + px as usize) * 4;
            let Some(p) = buf.get_mut(off..off + 4) else {
                continue;
            };
            // B,G,R 通道：out = bg*(1-α) + fg*α；A 恒 255（实底面板）
            p[0] = ((p[0] as u32 * (255 - a) + b * a) / 255) as u8;
            p[1] = ((p[1] as u32 * (255 - a) + g * a) / 255) as u8;
            p[2] = ((p[2] as u32 * (255 - a) + r * a) / 255) as u8;
            p[3] = 255;
        }
    }
}
