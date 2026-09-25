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

/// 模式标识字：常驻候选条的首项（`layout` 的第一项），中英切换瞬间另走
/// `chip_layout` 闪一次小窗。
pub fn mode_chip(chinese: bool) -> &'static str {
    if chinese {
        "中"
    } else {
        "英"
    }
}

/// 模式 chip 闪现窗的终宽：左右留白 + 字形实测宽，钳在**终宽 ≤ 8192**。
/// 钳位基准是 `create_buffer` 的守卫 `w > 8192`（守卫看的是终宽）：若钳字宽到 8192，
/// 终宽 = MARGIN_X*2 + 8192 = 8212，越过守卫。故字形宽先钳到 `8192 - MARGIN_X * 2`。
/// 正常单字 measure 仅 ~36px，钳位不触发；抽成纯函数只为钉住钳位基准（不依赖字体）。
pub fn chip_window_width(glyph_w: u32) -> u32 {
    MARGIN_X * 2 + glyph_w.min(8192 - MARGIN_X * 2)
}

/// 单个已摆放的项：x 为内容左沿，w 为实测宽；chip = 模式字（不编号、不可选）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedItem {
    pub x: u32,
    pub w: u32,
    pub text: String,
    pub chip: bool,
}

/// 一帧的完整摆放结果。空候选 → 1×1（隐藏帧，像素全透明）。
/// 有候选时首项是常驻模式字：它不占高亮下标，中英切换瞬间另走 `chip_layout`。
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

/// 冷路径探针：`Renderer::new()` 末尾用**真实渲染路径**（layout→measure 整形、
/// paint 再整形 + swash 光栅）跑一遍这段内容，把字体解析、缺字 fallback 选择、
/// shape-run 缓存与 image_cache 一次性填满。标签经 layout() 包成 `"N. <text>"`，
/// 前缀天然覆盖数字/点/空格（拉丁 + 标点）；扩展区稀有字故意写成 `\u{...}` 转义：
/// fontdb 无覆盖时会走 cosmic-text 全字体线扫，每个 face 首次要解析 CJK 大表 +
/// 建 harfrust shaper（实测 ~110ms 纯 CPU），这笔钱必须在进程启动期付掉，
/// 而不是首个 ACTIVATE 的按键同步路径上。
const WARMUP_PROBE: &[&str] = &[
    // 常用 CJK（含实测肇事页首字「能」，与测试字面同源）
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
    // 再抽 Ext-E / Ext-G 各一个，保证扫描面
    "\u{2C429}",
    "\u{30EDD}",
];

impl Renderer {
    /// FontSystem::new() 扫系统字体（fontconfig 路径），一次性、偏慢，只建一个。
    /// 构造末尾立刻 warmup()：把字形冷路径成本从按键路径挪到进程启动期
    /// （main 在 wayland 连接之前调用，守护进程启动期用户不可见）。
    pub fn new() -> Self {
        let mut this = Self {
            font_system: FontSystem::new(),
            cache: SwashCache::new(),
        };
        this.warmup();
        this
    }

    /// 冷路径预热：只填缓存的纯副作用，不改任何布局状态——warmup 之后的
    /// measure/layout 结果与未预热时逐位一致（缓存只省重算，不改字体选择）。
    /// 探针文本与真实 label 走同一套 Buffer::set_text(Shaping::Advanced) +
    /// shape_until_scroll + buffer.draw 路径；临时 scratch 画完即丢。
    /// 无字体环境（fontdb 为空）只是量不到字形：走 measure 既有的
    /// warn_no_font 分支，本函数不引入任何 unwrap/expect。
    fn warmup(&mut self) {
        let probe: Vec<String> = WARMUP_PROBE.iter().map(|s| s.to_string()).collect();
        let layout = self.layout(&probe, true);
        let mut scratch = vec![0u8; layout.pixel_len()];
        self.paint(&layout, 0, &mut scratch);
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

    /// 横排单行摆放：首项是常驻模式字（`chinese` = 当前中英模式，决定画「中」还是「英」），
    /// 其后每项 "N. 候选"，定宽分隔，总宽随内容自适应。
    /// candidates 已是当前页（调用方切好片），这里不再截断。
    /// 高度只含边距 + 行高：合成器已把 popup 摆在光标旁，表面内不再留光标行空行。
    /// 模式字常驻候选条首项（工单第 1 条）；`chip_layout` 只是切换瞬间的闪现小窗。
    /// 空候选 → 隐藏帧（模式字也不出现）。
    pub fn layout(&mut self, candidates: &[String], chinese: bool) -> Layout {
        if candidates.is_empty() {
            return Layout::hidden();
        }
        let mut items = Vec::with_capacity(candidates.len() + 1);
        let mut x = MARGIN_X;
        // 模式字压在最左：候选不重画的一刻也能一眼分清中英
        let chip = mode_chip(chinese).to_string();
        let chip_w = self.measure(&chip);
        items.push(PlacedItem {
            x,
            w: chip_w,
            text: chip,
            chip: true,
        });
        x += chip_w + SEP;
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
        // u32 累加无上限：候选极长/字形异常时 x 可能回绕成 0 或极小值，
        // create_pool 的 size 就与实际 shm 长度不符 → 合成器判 invalid arguments
        let width = (x - SEP + MARGIN_X).max(1).min(8192);
        let height = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
        Layout {
            width,
            height,
            items,
        }
    }

    /// 模式提示的一次性闪现小窗：只含 `中`/`英` 单字，下一次按键即清。
    /// 高度与候选条同律（MARGIN_Y*2 + 行高），宽度走 `chip_window_width`。
    pub fn chip_layout(&mut self, chinese: bool) -> Layout {
        let text = mode_chip(chinese).to_string();
        let height = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
        let measured = self.measure(&text);
        let w = measured.min(8192 - MARGIN_X * 2);
        Layout {
            width: chip_window_width(measured),
            height,
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
