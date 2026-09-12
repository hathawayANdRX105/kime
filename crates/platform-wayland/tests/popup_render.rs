//! 候选窗渲染纯函数测试（不连 wayland）：布局数学 + 像素输出。
//! 核心目标：抓到「从不 set_text 画空帧」「buffer 尺寸校验错」「高亮越界 panic」
//! 「布局里混进外部 gap 导致面板上空一行」这类只在真机上才暴露的 bug。

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

#[test]
fn mode_chip_differs_between_modes_and_is_nonempty() {
    assert!(!mode_chip(true).is_empty() && !mode_chip(false).is_empty());
    assert_ne!(mode_chip(true), mode_chip(false));
}

#[test]
fn chinese_empty_is_1x1_fully_transparent() {
    let mut r = Renderer::new();
    let l = r.layout(&[], true);
    assert!(l.is_hidden(), "中文 + 无候选必须就是隐藏帧");
    assert_eq!((l.width, l.height), (1, 1));
    assert_eq!(l.pixel_len(), 4);
    // 前置 0xFF 填充：证明 paint 真的写了透明，而不是测试自己留的
    let buf = paint(&mut r, &l, 0);
    assert_eq!(buf, vec![0u8; 4], "隐藏帧必须全 0（含 alpha=0）");
}

#[test]
fn english_empty_shows_chip_frame_not_transparent() {
    let mut r = Renderer::new();
    let l = r.layout(&[], false);
    assert!(!l.is_hidden(), "英文提示帧不能是隐藏帧");
    assert_eq!(l.items.len(), 1);
    assert!(l.items[0].chip);
    assert_eq!(l.items[0].text, mode_chip(false));
    // 「至少能容纳一个字」：宽度容得下边距之外的实测字宽
    assert!(l.width > MARGIN_X * 2, "提示帧过窄：{}", l.width);
    assert_eq!(l.height, MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32);
    assert!(l.pixel_len() > 4, "提示帧不能是 1×1");
    let buf = paint(&mut r, &l, 0);
    assert!(
        buf.chunks_exact(4).all(|p| p[3] == 255),
        "提示帧必须整幅不透明（alpha>0），否则用户看不见"
    );
    assert!(non_bg(&buf) > 0, "提示帧必须真的画出了字");
}

#[test]
fn height_has_no_external_gap_and_ignores_candidate_count() {
    let mut r = Renderer::new();
    let want = MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32;
    for n in [1usize, 3, 8, 20] {
        let l = r.layout(&cands(n), true);
        assert_eq!(l.height, want, "{n} 个候选的高度应恰为边距+行高");
    }
}

#[test]
fn chip_then_eight_candidates_layout_and_pixels() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(8), true);
    assert_eq!(l.items.len(), 9, "模式字 + 8 个候选");
    assert!(l.items[0].chip, "首项必须是模式字");
    assert_eq!(l.items[0].text, mode_chip(true));
    assert!(!l.items[1].chip);
    assert_eq!(
        l.items[1].text, "1. 候选1",
        "编号从第一个候选起，模式字不参与编号"
    );
    assert!(l.height > 1, "候选帧高必须 > 1");
    // 每项至少画出「N. 候选」的量级宽度
    assert!(
        l.items.iter().all(|it| it.w > 0),
        "存在宽度为 0 的项，疑似从不测量/从不 set_text"
    );
    // 不重叠 + 宽度公式：width = 左缘 + 字宽和 + 定宽分隔 + 右边距
    let mut prev_end = 0;
    for it in &l.items {
        assert!(it.x >= prev_end, "项 {it:?} 与前项重叠");
        prev_end = it.x + it.w;
    }
    assert_eq!(
        l.width,
        prev_end + MARGIN_X,
        "宽度 = Σ字宽 + 8×SEP + 2×MARGIN_X"
    );
    assert_eq!(
        l.width,
        l.items.iter().map(|it| it.w).sum::<u32>() + 8 * SEP + 2 * MARGIN_X
    );
    // 候选更多 → 更宽；同数量不同内容 → 不同宽度（宽度真来自文字测量）
    let more = r.layout(&cands(9), true);
    assert!(more.width > l.width, "候选数增加宽度未增");
    let short = r.layout(&vec!["一".to_string(); 8], true);
    assert!(short.width < l.width, "不同候选集宽度应不同");
    let buf = paint(&mut r, &l, 0);
    assert!(non_bg(&buf) > 0, "整帧只有背景 = 画了空帧");
    assert!(buf.chunks_exact(4).all(|p| p[3] == 255), "可见帧必须不透明");
}

#[test]
fn highlight_changes_pixels_and_out_of_range_is_safe() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(3), true);
    let base = paint(&mut r, &l, 0);
    let other = paint(&mut r, &l, 1);
    assert_ne!(base, other, "高亮项必须区别于普通项");
    // 越界：不着色、不 panic —— 帧里没有任何琥珀项，故与两个有高亮的帧都不同
    let oob = paint(&mut r, &l, 999);
    let none = paint(&mut r, &l, usize::MAX);
    assert_eq!(oob, none, "越界下标应一致地等同无高亮");
    assert_ne!(oob, base);
    assert_ne!(oob, other);
}

#[test]
fn chip_is_inked_and_never_highlighted() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(3), true);
    let w = l.width as usize;
    let strip = |buf: &[u8], idx: usize| -> Vec<u8> {
        let it = &l.items[idx];
        let (x0, x1) = (it.x as usize * 4, (it.x + it.w) as usize * 4);
        let mut out = Vec::new();
        for row in buf.chunks_exact(w * 4) {
            out.extend_from_slice(&row[x0..x1]);
        }
        out
    };
    let f0 = paint(&mut r, &l, 0);
    let f1 = paint(&mut r, &l, 1);
    let fnone = paint(&mut r, &l, usize::MAX);
    // 模式字不可选：它自己的像素区在三种高亮态下一字不差
    assert_eq!(strip(&f0, 0), strip(&f1, 0), "模式字区域不得随高亮改变");
    assert_eq!(strip(&f0, 0), strip(&fnone, 0), "模式字区域不得随高亮改变");
    // 模式字真的画出了墨迹（不是只占了背景）
    assert!(
        strip(&fnone, 0)
            .chunks_exact(4)
            .filter(|p| **p != BG)
            .count()
            > 0,
        "模式字区域只有背景 = 没画 chip"
    );
    // 对照：候选区随高亮变化（否则上面的相等是永真式）
    assert_ne!(strip(&f0, 1), strip(&f1, 1), "候选高亮必须改变候选区像素");
}
