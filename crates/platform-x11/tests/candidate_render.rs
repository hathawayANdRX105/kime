//! X11 候选窗纯像素渲染测试（不连 X11）：布局数学 + 像素输出。
//! 目标：抓住「空候选画了实底帧」「缓冲长度与宽高不符」「高亮越界 panic」
//! 这类只在真机上才暴露的 bug。本测试不创建任何 X11 资源。

use kime_core::Candidate;
use platform_x11::render::{LINE_HEIGHT, MARGIN_X, MARGIN_Y};
use platform_x11::Renderer;

/// 与 render.rs 同款背景 rgb(30,30,38)，B,G,R,A 序
const BG: [u8; 4] = [38, 30, 30, 255];

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        pinyin: String::new(),
        freq: 0,
        eff: 0,
        ai: false,
    }
}

fn cands(n: usize) -> Vec<Candidate> {
    (1..=n).map(|i| cand(&format!("候选{i}"))).collect()
}

fn non_bg(buf: &[u8]) -> usize {
    buf.chunks(4).filter(|p| *p != BG).count()
}

#[test]
fn empty_candidates_is_hidden_frame() {
    let mut r = Renderer::new();
    r.set_candidates(&[]);
    let f = r.render();
    assert!(f.is_hidden(), "空候选必须是隐藏帧");
    assert_eq!((f.width, f.height), (1, 1));
    assert_eq!(f.pixels.len(), 4);
    assert!(f.pixels.iter().all(|&p| p == 0), "隐藏帧像素必须全透明");
}

#[test]
fn invisible_window_is_hidden_even_with_candidates() {
    let mut r = Renderer::new();
    r.set_candidates(&[cand("候选")]);
    r.set_visible(false);
    let f = r.render();
    assert!(f.is_hidden(), "隐藏状态下不该产出实底帧");
}

#[test]
fn single_candidate_has_real_geometry_and_exact_buffer() {
    let mut r = Renderer::new();
    r.set_candidates(&[cand("你好")]);
    let f = r.render();
    assert!(!f.is_hidden());
    // 宽度至少是左右留白 + 一个字宽，高度恰为上下留白 + 行高
    assert!(f.width > MARGIN_X * 2, "候选帧过窄：{}", f.width);
    assert_eq!(
        f.height,
        MARGIN_Y * 2 + LINE_HEIGHT.ceil() as u32,
        "高度必须由真实布局算出"
    );
    // 缓冲长度严格等于 width*height*4（XPutImage 的 stride 契约）
    assert_eq!(f.pixels.len(), f.pixel_len());
    assert_eq!(f.pixels.len(), (f.width as usize) * (f.height as usize) * 4);
    // 实底面板：每像素 alpha 恒 255
    assert!(
        f.pixels.chunks(4).all(|p| p[3] == 255),
        "实底面板每像素必须不透明"
    );
}

#[test]
fn many_candidates_paint_ink_and_match_buffer_contract() {
    let mut r = Renderer::new();
    r.set_candidates(&cands(8));
    let f = r.render();
    assert!(!f.is_hidden());
    assert_eq!(f.pixels.len(), f.pixel_len());
    assert!(non_bg(&f.pixels) > 0, "整帧只有背景 = 画了空帧");
}

#[test]
fn width_grows_with_content_and_height_is_constant() {
    let mut r = Renderer::new();
    r.set_candidates(&[cand("一")]);
    let narrow = r.render();
    r.set_candidates(&[cand("一二三四五六七八九十")]);
    let wide = r.render();
    assert!(wide.width > narrow.width, "更长的候选文本宽度未增长");
    assert_eq!(narrow.height, wide.height, "高度与候选内容无关");
    assert!(narrow.width > MARGIN_X * 2 && wide.width > MARGIN_X * 2);
}

#[test]
fn highlight_differs_from_normal_and_out_of_range_is_safe() {
    let mut r = Renderer::new();
    r.set_candidates(&cands(3));
    let normal = r.render();
    r.set_highlight(1);
    let highlighted = r.render();
    assert_ne!(
        normal.pixels, highlighted.pixels,
        "高亮项像素必须区别于普通项"
    );
    // 越界高亮：不着色、不 panic，且帧里没有任何高亮项色
    r.set_highlight(99);
    let oob = r.render();
    assert_ne!(oob.pixels, normal.pixels);
    assert_ne!(oob.pixels, highlighted.pixels);
    assert_eq!(oob.pixels.len(), oob.pixel_len());
}
