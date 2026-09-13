//! press/release 配对回归：长按自动重复下，同一物理键可能中途从「被消费」变成「转发」
//! （组合恰好被删空的 Backspace 就是这一形态）。若转发 press 时不销掉旧的 consume 记账，
//! release 会被残留标记吃掉 → 应用以为键一直没松 → 客户端自带的长按重复无限退格（幽灵）。

use platform_wayland::SwallowTracker;

const KEY_BACKSPACE: u32 = 14;

#[test]
fn consumed_press_eats_exactly_one_release() {
    let mut t = SwallowTracker::default();
    t.consume(KEY_BACKSPACE);
    assert!(
        t.release(KEY_BACKSPACE),
        "press 被消费的键，release 必须吞掉"
    );
    assert!(
        !t.release(KEY_BACKSPACE),
        "吞一次即销账，不吞不存在的第二下"
    );
}

#[test]
fn forwarded_press_forwards_its_release_after_earlier_consume() {
    let mut t = SwallowTracker::default();
    t.consume(KEY_BACKSPACE); // 第 1 下：组合里删字符，被消费
    t.forward(KEY_BACKSPACE); // 第 2 下：组合已空，press 转给应用 —— 必须销记
    assert!(
        !t.release(KEY_BACKSPACE),
        "press 转发过的键，release 必须同样转发；残留记账 = 幽灵连发"
    );
}

#[test]
fn clear_drops_inflight_state_on_grab_change() {
    let mut t = SwallowTracker::default();
    t.consume(KEY_BACKSPACE);
    t.clear();
    assert!(!t.release(KEY_BACKSPACE), "grab 重建后旧在途键作废");
}
