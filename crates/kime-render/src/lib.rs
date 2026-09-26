//! 候选窗像素渲染共享内核：cosmic-text 横排单行 → ARGB8888 字节缓冲。
//!
//! 两个平台前端（`platform-wayland` 的 input-popup 与 `platform-x11` 的 XIM 候选窗）
//! 在排版参数、配色、字形测量、字形光栅这四件事上完全同款——同字号、同留白、同颜色、
//! 同 alpha 合成。历史上它们是复制粘贴的两份 render.rs，改一处必须记得同步另一处，
//! 已经漂移过一次（钳位一边用 `max().min()` 一边用 `clamp()`）。本 crate 把这份共用部分
//! 收成唯一实现，两个前端各自只保留真正不同的东西：候选窗的窗口管理、翻页与高亮语义、
//! 以及各自的 shm / X11 上屏路径。
//!
//! 内存序：ARGB8888 小端 = B,G,R,A。

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
/// 单帧边长上限（px）：两端上屏缓冲的尺寸守卫同值，越界会让 shm pool / XPutImage
/// 参数与实际缓冲不符，合成器直接判协议错误打死连接。
pub const MAX_FRAME_DIM: u32 = 8192;

/// 背景 rgb(30,30,38)，不透明。B,G,R,A 序。外部一律经 `fill_background` 取。
const BG: [u8; 4] = [38, 30, 30, 255];
/// 候选项常规色。
pub const FG: Color = Color::rgba(220, 220, 230, 255);
/// 高亮项：琥珀色，深底对比足够。
pub const HL: Color = Color::rgba(255, 220, 120, 255);

/// 候选条高度：上下留白 + 行高。合成器已把窗摆在光标旁，表面内不再留光标行空行。
pub fn panel_height() -> u32 {
    MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32
}

/// 候选条终宽：`x` 是最后一项右沿再加一个分隔宽。`x` 是 u32 无上限累加——候选极长 /
/// 字形异常时会回绕成 0 或极小值，尺寸就与实际缓冲不符，故钳在 `1..=MAX_FRAME_DIM`。
pub fn panel_width(x: u32) -> u32 {
    (x - SEP + MARGIN_X).clamp(1, MAX_FRAME_DIM)
}

/// 整帧铺底色。
pub fn fill_background(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&BG);
    }
}

/// 冷路径探针：`TextPainter::new()` 末尾用**真实渲染路径**（measure 整形 + draw_run
/// 再整形 + swash 光栅）跑一遍这段内容，把字体解析、缺字 fallback 选择、shape-run
/// 缓存与 image_cache 一次性填满。标签经调用方包成 `"N. <text>"`，前缀天然覆盖
/// 数字/点/空格（拉丁 + 标点）；扩展区稀有字故意写成 `\u{...}` 转义：fontdb 无覆盖时
/// 会走 cosmic-text 全字体线扫，每个 face 首次要解析 CJK 大表 + 建 harfrust shaper
/// （实测 ~110ms 纯 CPU），这笔钱必须在进程启动期付掉，而不是首个按键的同步路径上。
const WARMUP_PROBE: &[&str] = &[
    // 常用 CJK（含实测肇事页首字「能」）
    "能候选一啊中英",
    // 拉丁字母 + 数字（剪贴板候选可以是任意文本；label 前缀另带 "N. "）
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

/// 系统里连一个能用的字形都量不到：明确吼一声，别默默画空白帧。
fn warn_no_font(text: &str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("[kime-render] cosmic-text 量不到字形宽度（无可用字体？）：「{text}」");
    }
}

/// 横排单行文本的测量与光栅器：持有字体系统与 swash 缓存，产出 ARGB8888 字节。
///
/// 两端前端各建一个（互不共享进程），构造即预热，成本落在进程启动期。
pub struct TextPainter {
    font_system: FontSystem,
    cache: SwashCache,
}

impl Default for TextPainter {
    fn default() -> Self {
        Self::new()
    }
}

impl TextPainter {
    /// `FontSystem::new()` 扫系统字体（fontconfig 路径），一次性、偏慢，只建一个。
    /// 构造末尾立刻预热：把字形冷路径成本从按键路径挪到进程启动期。
    pub fn new() -> Self {
        let mut this = Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        };
        this.warmup();
        this
    }

    /// 测量一段文本的像素宽（横排、不换行）。
    /// 宽度取所有 run 的 `line_w` 最大值：script/BiDi 分段后各段不重叠，
    /// 单段宽度不是整行总宽。
    pub fn measure(&mut self, text: &str) -> u32 {
        let buffer = self.shape(text);
        let w = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0f32, f32::max);
        if w < 0.5 && !text.is_empty() {
            warn_no_font(text);
        }
        w.ceil().max(0.0) as u32
    }

    /// 把一段文本光栅进 `buf`：左沿 `x`，行盒在 `[MARGIN_Y, MARGIN_Y+行高)` 文本带内
    /// 垂直居中。`stride_px` 是 `buf` 每行的像素数。越界部分由 `composite_rect` 丢弃。
    pub fn draw_run(&mut self, text: &str, color: Color, buf: &mut [u8], stride_px: usize, x: i32) {
        let band_top = MARGIN_Y as i32;
        let band_h = LINE_HEIGHT.ceil() as i32;
        let mut buffer = self.shape(text);
        // 单行：把 run 的行盒垂直居中到文本带
        let dy = match buffer.layout_runs().next() {
            Some(run) => band_top + (band_h - run.line_height as i32) / 2 - run.line_top as i32,
            None => band_top,
        };
        buffer.draw(
            &mut self.font_system,
            &mut self.cache,
            color,
            |gx, gy, w, h, c| {
                composite_rect(buf, stride_px, (gx + x, gy + dy), (w, h), c);
            },
        );
    }

    /// 整形一段横排单行文本（measure 与光栅共用的那一段）。
    fn shape(&mut self, text: &str) -> Buffer {
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buffer.set_size(None, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    /// 冷路径预热：只填缓存的纯副作用，不改任何调用方状态——预热之后的
    /// measure/draw_run 结果与未预热时逐位一致（缓存只省重算，不改字体选择）。
    /// 探针文本与真实 label 走同一套整形 + 光栅路径；临时 scratch 画完即丢。
    /// 无字体环境（fontdb 为空）只是量不到字形：走 measure 既有的 warn_no_font
    /// 分支，本函数不引入任何 unwrap/expect。
    fn warmup(&mut self) {
        const SCRATCH_W: u32 = 256;
        const SCRATCH_H: u32 = 32;
        let mut scratch = vec![0u8; (SCRATCH_W * SCRATCH_H * 4) as usize];
        for text in WARMUP_PROBE {
            let label = format!("1. {text}");
            let _ = self.measure(&label);
            self.draw_run(&label, FG, &mut scratch, SCRATCH_W as usize, 0);
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
