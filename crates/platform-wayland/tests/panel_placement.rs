//! 候选面板落点计算。`place_cursor` 是纯函数（不连 wayland、不读 socket），
//! 这里只断言真实边界：贴光标下方、右边界左移、下边界翻到光标上方、翻完仍不出屏。

use platform_wayland::panel::{place_cursor, CaretRect};

const PW: f32 = 420.0;
const PH: f32 = 48.0;

fn caret(x: i32, y: i32, height: i32) -> CaretRect {
    CaretRect {
        x,
        y,
        height,
        ..screen(0, 0, 2560, 1440)
    }
}

fn screen(x: i32, y: i32, w: i32, h: i32) -> CaretRect {
    CaretRect {
        x: 0,
        y: 0,
        height: 0,
        screen_x: x,
        screen_y: y,
        screen_w: w,
        screen_h: h,
    }
}

#[test]
fn panel_sits_below_caret_row() {
    assert_eq!(place_cursor(&caret(100, 200, 20), PW, PH), (100.0, 220.0));
}

#[test]
fn right_overflow_shifts_panel_left() {
    // 2400 + 420 > 2560 → 左移到右边贴屏
    assert_eq!(place_cursor(&caret(2400, 200, 20), PW, PH), (2140.0, 220.0));
}

#[test]
fn bottom_overflow_flips_above_caret() {
    // 1400 + 20 + 48 > 1440 → 翻到光标行上沿
    assert_eq!(place_cursor(&caret(100, 1400, 20), PW, PH), (100.0, 1352.0));
}

#[test]
fn bottom_right_corner_clamps_both_axes() {
    assert_eq!(
        place_cursor(&caret(2500, 1435, 20), PW, PH),
        (2140.0, 1387.0)
    );
}

#[test]
fn flipped_panel_still_pinned_to_screen_top() {
    // 光标行高 1430：下方放不下（10+1430+48 > 1440），翻上方会出屏（10-48 < 0）→ 夹到 0
    assert_eq!(place_cursor(&caret(100, 10, 1430), PW, PH), (100.0, 0.0));
}

#[test]
fn clamps_against_the_monitor_the_caret_is_on() {
    // 第二块屏原点在 (2560,0)，4900+420 超出其右界 5120 → 4700；纵向 100+20+48 未越界
    let c = CaretRect {
        x: 4900,
        y: 100,
        height: 20,
        ..screen(2560, 0, 2560, 1440)
    };
    assert_eq!(place_cursor(&c, PW, PH), (4700.0, 120.0));
}

#[test]
fn oversized_panel_pins_to_screen_edges() {
    // 面板比屏宽还大：左移会把左边推出屏幕（200-420=-220），最终钉在屏幕左沿；
    // 纵向 10+20+48 未越 100，所以不落翻转分支
    let wide = CaretRect {
        x: 150,
        y: 10,
        height: 20,
        ..screen(0, 0, 200, 100)
    };
    assert_eq!(place_cursor(&wide, PW, PH), (0.0, 30.0));

    // 屏幕比面板还矮：下方放不下 → 翻上方（10-48=-38）→ 出屏 → 夹回屏幕顶
    let tall = CaretRect {
        x: 150,
        y: 10,
        height: 20,
        ..screen(0, 0, 200, 40)
    };
    assert_eq!(place_cursor(&tall, PW, PH), (0.0, 0.0));
}
