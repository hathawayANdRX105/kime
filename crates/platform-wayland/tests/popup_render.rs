//! 候选窗渲染纯函数测试（不连 wayland）：布局数学 + 像素输出。
//! 核心目标：抓到「从不 set_text 画空帧」「buffer 尺寸校验错」「高亮越界 panic」
//! 「模式字混进候选条」这类只在真机上才暴露的 bug。
//!
//! 契约（工单第 1 条重写）：候选条永远不含模式字；空候选 = 1×1 隐藏帧
//! （英文模式平时同样隐藏，没有常驻窗）；`中`/`英` 只活在切换瞬间的
//! chip_layout 小窗里。

use platform_wayland::render::mode_chip;
use platform_wayland::render::{LINE_HEIGHT, MARGIN_X, MARGIN_Y, SEP};
use platform_wayland::{Layout, Renderer};

const BG: [u8; 4] = [38, 30, 30, 255];

fn cands(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("候选{i}")).collect()
}

fn paint(r: &mut Renderer, l: &Layout, hl: usize) -> Vec<u8> {
    let mut buf = vec![0xFFu8; l.pixel_len()];
    r.paint(l, hl, &mut buf);
    buf
}

fn non_bg(buf: &[u8]) -> usize {
    buf.chunks_exact(4).filter(|p| *p != BG).count()
}

/// 模式字专用青绿色（CHIP = rgb(122,207,214)，ARGB8888 字节序 B,G,R,A → G 明显大于 R）。
/// FG 灰（r=g）、HL 琥珀（r>g）、BG（r=g）及其任意抗锯齿混色都够不到这个判据。
fn chip_pixels(buf: &[u8]) -> usize {
    buf.chunks_exact(4)
        .filter(|p| p[1] as i32 > p[2] as i32 + 40)
        .count()
}

#[test]
fn mode_chip_differs_between_modes_and_is_nonempty() {
    assert!(!mode_chip(true).is_empty() && !mode_chip(false).is_empty());
    assert_ne!(mode_chip(true), mode_chip(false));
}

#[test]
fn empty_candidates_is_1x1_fully_transparent() {
    let mut r = Renderer::new();
    let l = r.layout(&[]);
    assert!(l.is_hidden(), "空候选必须就是隐藏帧");
    assert_eq!((l.width, l.height), (1, 1));
    assert_eq!(l.pixel_len(), 4);
    let buf = paint(&mut r, &l, 0);
    assert!(buf.iter().all(|&b| b == 0), "隐藏帧像素必须全透明");
}

#[test]
fn candidate_bar_carries_no_mode_glyph() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(8));
    assert_eq!(l.items.len(), 8, "候选条里不允许混进模式字");
    assert!(l.items.iter().all(|i| !i.chip));
    assert_eq!(l.items[0].text, "1. 候选1", "首项就是首个候选");
    let buf = paint(&mut r, &l, 0);
    assert!(non_bg(&buf) > 0, "整帧只有背景 = 画了空帧");
    assert_eq!(chip_pixels(&buf), 0, "候选条不得出现模式字色像素");
}

#[test]
fn chip_window_is_single_glyph_sized() {
    let mut r = Renderer::new();
    for chinese in [true, false] {
        let l = r.chip_layout(chinese);
        assert_eq!(l.items.len(), 1);
        assert!(l.items[0].chip);
        assert_eq!(l.items[0].text, mode_chip(chinese));
        assert!(l.width > MARGIN_X * 2, "闪现窗过窄：{}", l.width);
        assert_eq!(
            l.width,
            MARGIN_X * 2 + l.items[0].w,
            "宽度必须恰为单字+留白"
        );
        assert_eq!(l.height, MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32);
    }
}

#[test]
fn height_has_no_external_gap_and_ignores_candidate_count() {
    let mut r = Renderer::new();
    let want = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
    for n in [1usize, 3, 8, 20] {
        let l = r.layout(&cands(n));
        assert_eq!(l.height, want, "{n} 个候选的高度应恰为边距+行高");
    }
}

#[test]
fn width_grows_with_content() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(8));
    let more = r.layout(&cands(9));
    assert!(more.width > l.width, "候选数增加宽度未增");
    let short = r.layout(&vec!["一".to_string(); 8]);
    assert!(short.width < l.width, "不同候选集宽度应不同");
    // 首项左沿 = 留白，第二项至少隔一个分隔宽
    assert_eq!(l.items[0].x, MARGIN_X);
    assert!(l.items[1].x >= l.items[0].x + l.items[0].w + SEP);
}

#[test]
fn highlight_changes_pixels_and_out_of_range_is_safe() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(3));
    let base = paint(&mut r, &l, 0);
    let other = paint(&mut r, &l, 1);
    assert_ne!(base, other, "高亮项必须区别于普通项");
    // 越界：不着色、不 panic —— 帧里没有任何琥珀项，与有高亮的帧都不同
    let oob = paint(&mut r, &l, 99);
    assert_ne!(oob, base);
    assert_ne!(oob, other);
}

#[test]
fn chip_is_inked_and_never_highlighted() {
    let mut r = Renderer::new();
    let l = r.chip_layout(false);
    let buf = paint(&mut r, &l, 0);
    assert!(non_bg(&buf) > 0, "闪现窗只有背景 = 没画上字");
    assert!(chip_pixels(&buf) > 0, "模式字必须用 CHIP 色画上");
    assert_eq!(buf, paint(&mut r, &l, 5), "闪现窗没有候选，高亮无意义");
}

/// 崩溃复现路径回归：候选窗宽度是 u32 累加，极长候选/字形异常时可能回绕，
/// 一旦回绕成 0 或与实际 shm 长度不符，合成器就报 wl_shm.create_pool
/// invalid arguments 并杀掉整条连接（kime-ime 进程死亡，表现为"打着打着突然没输入法"）。
/// layout 出口必须钳到 create_buffer 的同款上限内，两处上限必须一致。
#[test]
fn layout_width_never_exceeds_pool_guard() {
    let mut r = Renderer::new();
    // 超长候选：单条 5000 字，强制 x 累加远超 8192
    let huge = vec!["啊".repeat(5000)];
    let l = r.layout(&huge);
    assert!(
        l.width <= 8192,
        "layout 宽度 {} 超过 create_buffer 守卫上限，pool size 会与 shm 长度不符",
        l.width
    );
    assert!(l.width >= 1, "宽度回绕成 0 会直接触发尺寸非法");
    // pixel_len 必须与 create_buffer 的 len = w*h*4 同构，不得溢出
    assert_eq!(l.pixel_len(), (l.width as usize) * (l.height as usize) * 4);

    // 多条超长候选同样受钳
    let many = (0..8).map(|_| "啊".repeat(2000)).collect::<Vec<_>>();
    let l = r.layout(&many);
    assert!(l.width <= 8192, "多候选累加后宽度 {} 仍然越界", l.width);

    // chip 小窗同律
    let c = r.chip_layout(true);
    assert!(c.width <= 8192, "chip 宽度 {} 越界", c.width);
}
