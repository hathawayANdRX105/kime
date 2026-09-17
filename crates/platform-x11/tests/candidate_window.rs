//! X11 candidate-window placement tests. These exercise pure geometry only and
//! do not require a running X server.

use platform_x11::window::{place_window, SPOT_DX, SPOT_DY, SPOT_UP_CLEARANCE};

#[test]
fn places_window_below_and_right_of_cursor_by_default() {
    assert_eq!(
        place_window((100, 80), (240, 40), (1920, 1080)),
        (100 + SPOT_DX, 80 + SPOT_DY)
    );
}

#[test]
fn pushes_window_left_when_right_edge_would_overflow() {
    let position = place_window((1850, 100), (300, 40), (1920, 1080));
    assert_eq!(position, (1620, 100 + SPOT_DY));
}

#[test]
fn flips_window_above_cursor_when_bottom_edge_would_overflow() {
    let position = place_window((500, 1060), (300, 60), (1920, 1080));
    assert_eq!(position, (500 + SPOT_DX, 1060 - SPOT_UP_CLEARANCE - 60));
}

#[test]
fn clamps_negative_or_offscreen_cursor_to_visible_area() {
    assert_eq!(place_window((-200, -100), (200, 40), (1920, 1080)), (0, 0));
    assert_eq!(
        place_window((5000, 5000), (200, 40), (1920, 1080)),
        (1720, 1080 - 40)
    );
}

#[test]
fn oversized_window_degrades_to_top_left_without_negative_coordinates() {
    assert_eq!(place_window((900, 500), (3000, 2000), (1920, 1080)), (0, 0));
}

#[test]
fn repeated_identical_inputs_have_identical_geometry() {
    let first = place_window((640, 480), (420, 46), (1920, 1080));
    let second = place_window((640, 480), (420, 46), (1920, 1080));
    assert_eq!(first, second);
}
