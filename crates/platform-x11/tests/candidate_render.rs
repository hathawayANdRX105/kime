//! X11 候选窗纯像素渲染测试（不连 X11）：布局数学 + 像素输出。
//! 目标：抓住「空候选画了实底帧」「缓冲长度与宽高不符」「高亮越界 panic」
//! 这类只在真机上才暴露的 bug。本测试不创建任何 X11 资源。
//!
//! 候选条第一项是常驻模式字（CHIP 色）：不可选、也永不高亮，
//! 高亮下标只数候选（模式字不占号）。

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

/// 模式字专用青绿色（CHIP = rgb(122,207,214)，ARGB8888 字节序 B,G,R,A → G 明显大于 R）。
/// FG 灰（r=g）、HL 琥珀（r>g）、BG（r=g）及其任意抗锯齿混色都够不到这个判据。
fn chip_pixels(buf: &[u8]) -> usize {
    buf.chunks(4)
        .filter(|p| p[1] as i32 > p[2] as i32 + 40)
        .count()
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

#[test]
fn candidate_bar_starts_with_mode_chip() {
    let mut r = Renderer::new();
    r.set_chinese(true);
    r.set_candidates(&cands(3));
    let f = r.render();
    assert!(!f.is_hidden());
    assert!(chip_pixels(&f.pixels) > 0, "候选条头部必须上 CHIP 色");
}

#[test]
fn candidate_bar_chip_follows_mode() {
    let mut r = Renderer::new();
    r.set_candidates(&cands(3));
    r.set_chinese(true);
    let zh = r.render();
    r.set_chinese(false);
    let en = r.render();
    assert!(chip_pixels(&zh.pixels) > 0 && chip_pixels(&en.pixels) > 0);
    // 帧比对按字形可区分性开关：CI 的 ubuntu-latest 没装 CJK 字体，「中」「英」
    // 双双回退到同一个 .notdef 字形，两帧逐字节相同（wayland 侧 run 36062714042
    // 实测）。先用另一个候选集探测这套字体到底能不能区分二者。
    let mut probe = Renderer::new();
    probe.set_candidates(&[cand("一")]);
    probe.set_chinese(true);
    let probe_zh = probe.render();
    probe.set_chinese(false);
    let probe_en = probe.render();
    if probe_zh.pixels != probe_en.pixels {
        assert!(
            zh.pixels != en.pixels,
            "字体能区分中/英时两态必须画出不同的帧"
        );
    }
}

#[test]
fn chip_is_never_highlighted() {
    let mut r = Renderer::new();
    r.set_candidates(&cands(3));
    r.set_highlight(0);
    let hl0 = r.render();
    // 越界高亮：候选全回 FG，模式字仍应是唯一的非灰/非琥珀色块
    r.set_highlight(99);
    let oob = r.render();
    assert!(chip_pixels(&oob.pixels) > 0, "越界高亮不得抹掉模式字");
    assert_eq!(
        chip_pixels(&oob.pixels),
        chip_pixels(&hl0.pixels),
        "模式字像素数与高亮无关"
    );
    assert_ne!(
        hl0.pixels, oob.pixels,
        "高亮 0 与越界高亮必须画出不同的帧（候选 0 的 HL 生效）"
    );
}

/// 超长候选不得把帧宽顶过 8192（钳位）更不得触到 u16 上限 65535（回绕打死连接）。
/// 与 wayland 侧 `layout_width_never_exceeds_pool_guard` 同律：出口钳 8192，
/// 远低于 put_image 的 u16 参数上限，`as u16` 永不回绕。
#[test]
fn frame_width_never_exceeds_u16_guard() {
    // 单条数千汉字：远超 8192
    let mut r = Renderer::new();
    r.set_candidates(&[cand(&"啊".repeat(3000))]);
    let f = r.render();
    assert!(
        (1..=8192).contains(&f.width),
        "超长候选帧宽 {} 必须落在 1..=8192（钳位生效且不为 0）",
        f.width
    );
    assert!(
        f.width <= u16::MAX as u32,
        "帧宽 {} 不得越 u16 上限",
        f.width
    );
    // 缓冲长度与 width*height*4 自洽（XPutImage stride 契约）
    assert_eq!(f.pixels.len(), f.pixel_len());
    assert_eq!(f.pixels.len(), (f.width as usize) * (f.height as usize) * 4);

    // 多条各数千字：累加后同样受钳
    let many: Vec<Candidate> = (0..8)
        .map(|i| cand(&format!("{}. {}", i + 1, "啊".repeat(1500))))
        .collect();
    let mut r = Renderer::new();
    r.set_candidates(&many);
    let f = r.render();
    assert!(f.width <= 8192, "多候选累加后帧宽 {} 仍越界", f.width);
    assert_eq!(f.pixels.len(), (f.width as usize) * (f.height as usize) * 4);
}
