//! 邻键纠错（全拼）：按键串的编辑距离 1 变体生成。
//!
//! 只处理两类真实高频错键：① 邻键替换（物理相邻按错，`zhant`→`zhang`）；
//! ② 相邻转位（顺序敲反，`xain`→`xian`——拼音里最经典的错法）。不做
//! insertion/deletion（那类错误与音节切分纠缠，误纠率高，v1 不碰）。
//!
//! 纠错在**按键串层**做而不是音节层：错误键串往往根本切不出音节
//! （`xain` segment 失败），音节层无从谈起。变体生成后由 engine 复用
//! 整条现有管线（segment → lookup_prefix），零新查询路径。
//!
//! 调用方只在直查候选不足时调用（正常输入零开销）；纠错候选永远排在
//! 精确结果之后（engine 侧 append 去重），不改变无纠错时的任何行为。

/// QWERTY 物理邻接（正交 + 斜向），小写字母。
const NEIGHBORS: &[(&str, &str)] = &[
    ("q", "wa"),
    ("w", "qesa"),
    ("e", "wrsd"),
    ("r", "edtf"),
    ("t", "ryfg"),
    ("y", "tugh"),
    ("u", "yihj"),
    ("i", "uojk"),
    ("o", "iklp"),
    ("p", "ol"),
    ("a", "qwsz"),
    ("s", "awedxz"),
    ("d", "serfcx"),
    ("f", "drtgvc"),
    ("g", "ftyhbv"),
    ("h", "gyujnb"),
    ("j", "huikmn"),
    ("k", "jiolm"),
    ("l", "kop"),
    ("z", "asx"),
    ("x", "zsdc"),
    ("c", "xdfv"),
    ("v", "cfgb"),
    ("b", "vghn"),
    ("n", "bhjm"),
    ("m", "njk"),
];

fn neighbor_chars(b: u8) -> impl Iterator<Item = u8> {
    let key = b as char;
    let value: &'static str = NEIGHBORS
        .iter()
        .find(|(k, _)| k.chars().next() == Some(key))
        .map(|(_, v)| *v)
        .unwrap_or("");
    value.bytes().filter(|c| c.is_ascii_lowercase())
}

/// 对按键串生成编辑距离 1 的变体：每位置邻键替换 + 相邻转位。
/// 全 ASCII 小写输入；输出确定性排序（先替换按位置序，后转位按位置序），已去重。
pub fn corrected_keys(letters: &str) -> Vec<String> {
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_lowercase()) {
        return Vec::new();
    }
    let bytes: Vec<u8> = letters.bytes().collect();
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 邻键替换
    for (i, &b) in bytes.iter().enumerate() {
        for nb in neighbor_chars(b) {
            let mut v = bytes.clone();
            if v[i] == nb {
                continue;
            }
            v[i] = nb;
            // 全 ASCII，from_utf8 不可能失败
            let s = String::from_utf8(v).expect("ascii");
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
    }
    // 相邻转位（相同字母交换无意义）
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == bytes[i + 1] {
            continue;
        }
        let mut v = bytes.clone();
        v.swap(i, i + 1);
        let s = String::from_utf8(v).expect("ascii");
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// 直查候选低于该数时才触发纠错（正常输入零开销的开关）。
pub(crate) const CORRECTION_TRIGGER_MIN: usize = 5;

/// 纠错输入长度上限（字母）：6 音节全拼 = 12。更长的输入是句子，
/// 容错由 Viterbi 多切分承担，编辑距离枚举在那里只有成本没有收益。
pub(crate) const MAX_CORRECTION_INPUT: usize = 12;
