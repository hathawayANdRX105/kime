//! X11 候选窗纯像素渲染：cosmic-text 横排候选 → ARGB8888 字节缓冲。
//!
//! 内存序：ARGB8888 小端 = B,G,R,A，可直接喂 XPutImage。
//! 本模块不碰 X11：无连接、无窗口、无绘图调用（那是 window.rs/T3 的事），
//! 布局与光栅都是纯计算，tests/candidate_render.rs 直接对返回值断言。
//!
//! 排版参数与配色跟 platform-wayland 候选条同款（同字号、同留白、同颜色）。

use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use kime_core::Candidate;

pub const FONT_SIZE: f32 = 16.0;
pub const LINE_HEIGHT: f32 = 22.0;
/// 面板左右留白
pub const MARGIN_X: u32 = 10;
/// 上下留白
pub const MARGIN_Y: u32 = 6;
/// 候选项之间的定宽分隔（px）
pub const SEP: u32 = 10;

/// 背景 rgb(30,30,38)，不透明。B,G,R,A 序。
const BG: [u8; 4] = [38, 30, 30, 255];
const FG: Color = Color::rgba(220, 220, 230, 255);
/// 高亮项：琥珀色，深底对比足够
const HL: Color = Color::rgba(255, 220, 120, 255);

/// 冷路径探针：与 platform-wayland/src/render.rs 的 WARMUP_PROBE 同源同款
/// （两处 render.rs 是复制粘贴关系，改一处须同步另一处）。构造期用**真实渲染
/// 路径**（Buffer::set_text(Shaping::Advanced) → shape_until_scroll → draw）
/// 跑一遍，把字体解析、缺字 fallback 全字体线扫、shaper 懒建与 swash image
/// 缓存一次性填满。扩展区稀有字写成 `\u{...}`：fontdb 无覆盖时每个 face 首次
/// 要解析 CJK 大表（实测 ~110ms 纯 CPU），这笔钱必须在 XIM 进程启动期付掉，
/// 而不是首个中文按键的同步路径上。
const WARMUP_PROBE: &[&str] = &[
    // 常用 CJK（含实测肇事页首字「能」）
    "能候选一啊中英",
    // 拉丁字母 + 数字（label 前缀另带 "N. "）
    "abc123",
    // Ext-A 实测肇事字：㲌 㴰 䏻 䘅
    "\u{3C8C}",
    "\u{3D30}",
    "\u{43FB}",
    "\u{4605}",
    // 螚（U+879A，URO 内稀有字，实测肇事）
    "\u{879A}",
    // Ext-B 抽样：𠹌 𢆂
    "\u{20E4C}",
    "\u{22182}",
    // Ext-E / Ext-G 各抽一个，保证扫描面
    "\u{2C429}",
    "\u{30EDD}",
];

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
    font_system: FontSystem,
    cache: SwashCache,
}
impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    /// FontSystem::new() 扫系统字体（fontconfig 路径），一次性、偏慢，只建一个。
    /// 构造末尾立刻 warmup()：Renderer 由 X11IM::new 在进事件循环前建好，
    /// 预热成本落在进程启动期而非 XIM 按键同步路径。
    pub fn new() -> Self {
        let mut this = Self {
            candidates: Vec::new(),
            highlight: 0,
            visible: false,
            x: 0,
            y: 0,
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        };
        this.warmup();
        this
    }

    /// 冷路径预热：纯副作用，只填字体/shape/swash 缓存，不改任何布局状态——
    /// warmup 之后的渲染结果与未预热时逐位一致。探针走 render() 同一套
    /// measure + Buffer::draw 路径，临时 scratch 画完即丢；无字体环境
    /// （fontdb 为空）只是量不到字形，不引入 unwrap/expect。
    fn warmup(&mut self) {
        const SCRATCH_W: u32 = 256;
        const SCRATCH_H: u32 = 32;
        let mut pixels = vec![0u8; (SCRATCH_W * SCRATCH_H * 4) as usize];
        let band_top = MARGIN_Y as i32;
        let band_h = LINE_HEIGHT.ceil() as i32;
        for text in WARMUP_PROBE {
            let label = format!("1. {}", text);
            let _ = self.measure(&label);
            let mut buffer =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buffer.set_size(None, None);
            buffer.set_text(&label, &Attrs::new(), Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
            let dy = match buffer.layout_runs().next() {
                Some(run) => band_top + (band_h - run.line_height as i32) / 2 - run.line_top as i32,
                None => band_top,
            };
            buffer.draw(
                &mut self.font_system,
                &mut self.cache,
                FG,
                |gx, gy, w, h, c| {
                    composite_rect(&mut pixels, SCRATCH_W as usize, (gx, gy + dy), (w, h), c);
                },
            );
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
            let w = self.measure(label);
            items.push((x, label.clone()));
            x += w + SEP;
        }
        let width = (x - SEP + MARGIN_X).clamp(1, 8192);
        let height = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for px in pixels.chunks_mut(4) {
            px.copy_from_slice(&BG);
        }
        let band_top = MARGIN_Y as i32;
        let band_h = LINE_HEIGHT.ceil() as i32;
        for (idx, (item_x, label)) in items.iter().enumerate() {
            let color = if idx == self.highlight { HL } else { FG };
            let mut buffer =
                Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            buffer.set_size(None, None);
            buffer.set_text(label, &Attrs::new(), Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
            // 单行：把 run 的行盒垂直居中到文本带
            let dy = match buffer.layout_runs().next() {
                Some(run) => band_top + (band_h - run.line_height as i32) / 2 - run.line_top as i32,
                None => band_top,
            };
            let ox = *item_x as i32;
            buffer.draw(
                &mut self.font_system,
                &mut self.cache,
                color,
                |gx, gy, w, h, c| {
                    composite_rect(&mut pixels, width as usize, (gx + ox, gy + dy), (w, h), c);
                },
            );
        }

        RenderedFrame {
            width,
            height,
            pixels,
        }
    }

    /// 测量一段文本的像素宽（横排、不换行）。
    /// 宽度取所有 run 的 `line_w` 最大值：script/BiDi 分段后各段不重叠，
    /// 单段宽度不是整行总宽。
    fn measure(&mut self, text: &str) -> u32 {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buffer.set_size(None, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0f32, f32::max)
            .ceil()
            .max(0.0) as u32
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
