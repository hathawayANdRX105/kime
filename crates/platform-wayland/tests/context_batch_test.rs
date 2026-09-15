//! input-method-v2 done 批处理的状态机测试（第六轮 WaylandShell 轨）。
//!
//! 协议事件流是「暂存 → done 提交」的双缓冲：surrounding_text /
//! text_change_cause / content_type 只改 pending，done 才生效。纯决策被抽到
//! [`platform_wayland::context_batch::plan_context_commit`]，这里用假数据把
//! 每一条路径钉住——回声过滤、归一拒收、字符截断、空批保持。
//! dispatch 层薄到不需要 wayland 连接就能推理（同族先例：tests/key_routing.rs）。

use platform_wayland::context_batch::{plan_context_commit, ContextCommit, CONTEXT_TAIL_CHARS};

/// 假引擎：只记录被推过来的上下文，测试看 set_context 的调用序列。
#[derive(Default)]
struct Recorder {
    contexts: Vec<Option<String>>,
}

impl Recorder {
    /// dispatch 层拿到裁决后对引擎执行的动作：只有 Apply 才调 set_context。
    fn apply(&mut self, action: &ContextCommit) {
        match action {
            ContextCommit::Apply(tail) => self.contexts.push(tail.clone()),
            // Echo（回声）与 Unchanged（本批无 surrounding）都不推引擎
            _ => {}
        }
    }
}

fn surrounding(text: &str, cursor: usize) -> (String, usize) {
    (text.to_string(), cursor)
}

#[test]
fn done_commits_tail_before_cursor() {
    // 在「你好」后面打字：光标在「你好」之后 = 6 字节（2 个汉字）
    let s = surrounding("你好", 6);
    let action = plan_context_commit(Some(&s), 1, CONTEXT_TAIL_CHARS);
    assert_eq!(action, ContextCommit::Apply(Some("你好".to_string())));
}

#[test]
fn echo_cause_zero_skips_engine() {
    // cause=0 = INPUT_METHOD：自己上屏的回声。字段照存，但不推引擎——
    // 否则输入法把自己的上屏当作用户输入，构成自激循环。
    let s = surrounding("你好", 6);
    let action = plan_context_commit(Some(&s), 0, CONTEXT_TAIL_CHARS);
    assert!(matches!(action, ContextCommit::Echo));

    let mut rec = Recorder::default();
    rec.apply(&action);
    assert!(rec.contexts.is_empty(), "回声不得推引擎");
}

#[test]
fn non_echo_cause_commits() {
    // cause≠0（应用侧编辑）→ 正常提交
    let s = surrounding("abc", 3);
    let action = plan_context_commit(Some(&s), 1, CONTEXT_TAIL_CHARS);
    assert_eq!(action, ContextCommit::Apply(Some("abc".to_string())));
}

#[test]
fn tail_truncates_to_max_chars_by_character() {
    // 40 个汉字、光标在末尾 → 只留最后 32 个；截断按字符边界，不按字节
    let text = "字".repeat(40);
    let s = surrounding(&text, text.len());
    let action = plan_context_commit(Some(&s), 1, CONTEXT_TAIL_CHARS);
    let ContextCommit::Apply(Some(tail)) = action else {
        panic!("应提交尾巴: {action:?}");
    };
    assert_eq!(tail.chars().count(), CONTEXT_TAIL_CHARS);
    assert!(text.ends_with(&tail));
}

#[test]
fn cursor_inside_char_rejects_and_clears_context() {
    // cursor 劈开「你」的 UTF-8 编码（落在第 1 字节）→ 归一失败 → 明确 None。
    // 覆盖旧尾巴，不留在途上下文把候选带偏。
    let s = surrounding("你好", 1);
    let action = plan_context_commit(Some(&s), 1, CONTEXT_TAIL_CHARS);
    assert_eq!(action, ContextCommit::Apply(None));
}

#[test]
fn cursor_at_start_yields_none() {
    // 光标紧贴句首：尾巴为空 → None。注意与 Unchanged 的区别——
    // surrounding_text 事件来过，是「明确没有上下文」，不是「本批没变化」。
    let s = surrounding("你好", 0);
    let action = plan_context_commit(Some(&s), 1, CONTEXT_TAIL_CHARS);
    assert_eq!(action, ContextCommit::Apply(None));
}

#[test]
fn batch_without_surrounding_keeps_context() {
    // done 也会因 content_type 等单独到达：本批没来 surrounding_text，
    // 合成器没说光标位置变了，旧上下文保持不动。
    let action = plan_context_commit(None, 1, CONTEXT_TAIL_CHARS);
    assert_eq!(action, ContextCommit::Unchanged);
}

#[test]
fn echo_wins_over_missing_surrounding() {
    // 回声优先裁决：即使本批一个事件没有，cause=0 也不推引擎
    let action = plan_context_commit(None, 0, CONTEXT_TAIL_CHARS);
    assert!(matches!(action, ContextCommit::Echo));
}

/// 真机事件序列：应用编辑提交一次，随后自己上屏触发回声 done。
/// 引擎看到的 set_context 序列必须只有第一条——这是回声过滤的端到端契约。
#[test]
fn event_sequence_apply_then_echo() {
    let mut rec = Recorder::default();

    // 第 1 批：应用侧编辑，光标前是「今天」（6 字节）
    let s1 = surrounding("今天", 6);
    rec.apply(&plan_context_commit(Some(&s1), 1, CONTEXT_TAIL_CHARS));

    // 第 2 批：自己 commit 了「今天天气」，合成器回弹的 surrounding + cause=0
    let s2 = surrounding("今天天气", 12);
    rec.apply(&plan_context_commit(Some(&s2), 0, CONTEXT_TAIL_CHARS));

    assert_eq!(rec.contexts, vec![Some("今天".to_string())]);
}

/// 归一失败后下一次有效 surrounding 必须能重新建立上下文
/// （None 不会把状态机卡死）。
#[test]
fn recovery_after_rejected_cursor() {
    let mut rec = Recorder::default();
    rec.apply(&plan_context_commit(
        Some(&surrounding("你好", 1)),
        1,
        CONTEXT_TAIL_CHARS,
    ));
    rec.apply(&plan_context_commit(
        Some(&surrounding("你好世界", 6)),
        1,
        CONTEXT_TAIL_CHARS,
    ));
    assert_eq!(rec.contexts, vec![None, Some("你好".to_string())]);
}
