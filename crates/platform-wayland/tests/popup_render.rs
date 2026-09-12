//! 候选窗渲染纯函数测试（不连 wayland）：布局数学 + 像素输出。
//! 核心目标：抓到「从不 set_text 画空帧」「buffer 尺寸校验错」「高亮越界 panic」
//! 这类只在真机上才暴露的 bug。

use platform_wayland::render::{MARGIN_X, SEP};
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
fn empty_candidates_is_1x1_fully_transparent() {
    let mut r = Renderer::new();
    let l = r.layout(&[], 0);
    assert_eq!((l.width, l.height), (1, 1));
    assert_eq!(l.pixel_len(), 4);
    // 前置 0xFF 填充：证明 paint 真的写了透明，而不是测试自己留的
    let buf = paint(&mut r, &l, 0);
    assert_eq!(buf, vec![0u8; 4], "隐藏帧必须全 0（含 alpha=0）");
}

#[test]
fn eight_candidates_widen_and_advance_without_overlap() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(8), 0);
    assert!(l.height > 1, "候选帧高必须 > 1");
    assert_eq!(l.items.len(), 8);
    // 每项至少画出「N. 候选」的量级宽度；总宽含边距与定宽分隔
    let lower = 8 * MARGIN_X + 7 * SEP;
    assert!(
        l.width > lower && l.items.iter().all(|it| it.w > 0),
        "宽度 {w} 异常，疑似从不测量/从不 set_text",
        w = l.width
    );
    let mut prev_end = 0;
    for it in &l.items {
        assert!(it.x >= prev_end, "项 {it:?} 与前项重叠");
        prev_end = it.x + it.w;
    }
    assert!(l.width >= prev_end + MARGIN_X);
    // 候选更多 → 更宽
    let more = r.layout(&cands(9), 0);
    assert!(more.width > l.width, "候选数增加宽度未增");
    // 同数量不同内容 → 不同宽度（证明宽度真来自文字测量）
    let short = r.layout(&vec!["一".to_string(); 8], 0);
    assert!(short.width < l.width, "不同候选集宽度应不同");
    // 有候选的帧：像素确实画了字，且整条不透明
    let buf = paint(&mut r, &l, 0);
    assert!(non_bg(&buf) > 0, "整帧只有背景 = 画了空帧");
    assert!(buf.chunks_exact(4).all(|p| p[3] == 255), "可见帧必须不透明");
}

#[test]
fn highlight_changes_pixels_and_out_of_range_is_safe() {
    let mut r = Renderer::new();
    let l = r.layout(&cands(3), 4);
    assert_eq!(l.height, 4 + platform_wayland::render::MARGIN_Y * 2 + 22);
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
