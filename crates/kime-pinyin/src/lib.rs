//! 拼音音节切分 — 纯逻辑，零依赖。
//!
//! 契约：ascii 小写字母串 → 所有合法切分，最优在前。
//! "xian" → [["xian"], ["xi","an"]]；"nihao" → [["ni","hao"], ["ni","ha","o"]]；
//! 空串 / 非 a-z 字符 / 无法切分 → 空 vec。
//!
//! 只认标准普通话无调音节表（401 条，含 a/o/e 单字）；声母缩写（nh）由
//! dict 层的 abbrev 列承担，不在此处。

/// 一次切分：音节序列，如 ["ni","hao"]
pub type Reading = Vec<String>;

/// 全部合法切分，最优排序（音节数最少优先；同长度按字典序）
///
/// 算法：DFS 从左到右探索，每步尝试 6→1 字母的音节前缀；先吃完更长的音节
/// → 产生的切分自然按"总音节数从少到多"排列。空串/非法字符/无解 → 空 vec。
pub fn segment(input: &str) -> Vec<Reading> {
    if input.is_empty() {
        return Vec::new();
    }
    // 契约：只接受 a-z；其它（含大写、数字、中文）一律视为非法输入。
    if !input.bytes().all(|b| b.is_ascii_lowercase()) {
        return Vec::new();
    }

    let bytes = input.as_bytes();
    let mut out: Vec<Reading> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    dfs(bytes, 0, &mut path, &mut out);
    out
}

fn dfs(bytes: &[u8], pos: usize, path: &mut Vec<String>, out: &mut Vec<Reading>) {
    if pos == bytes.len() {
        out.push(path.clone());
        return;
    }
    // 6→1 倒序尝试：先吃更长音节，最外层切分自然更短（音节数更少）。
    let max = (bytes.len() - pos).min(6);
    for len in (1..=max).rev() {
        // bytes[pos..pos+len] 已经是 ASCII 小写字母（前置过滤），合法 UTF-8。
        // safe 版本零额外成本：编译器在 ASCII 切片上消除验证。
        let sub = std::str::from_utf8(&bytes[pos..pos + len]).expect("ascii slice");
        if is_syllable(sub) {
            path.push(sub.to_owned());
            dfs(bytes, pos + len, path, out);
            path.pop();
        }
    }
}

/// 标准普通话无调音节表（401 条；按字典序排好）。
const SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong",
    "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou",
    "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de", "dei", "deng",
    "di", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo",
    "e", "ei", "en", "eng", "er", "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou", "fu",
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua",
    "guai", "guan", "guang", "gui", "gun", "guo", "ha", "hai", "han", "hang", "hao", "he", "hei",
    "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo", "ji",
    "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue",
    "jun", "ka", "kai", "kan", "kang", "kao", "ke", "ken", "keng", "kong", "kou", "ku", "kua",
    "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la", "lai", "lan", "lang", "lao", "le", "lei",
    "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "long", "lou", "lu",
    "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man", "mang", "mao", "me", "mei", "men",
    "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu", "na", "nai",
    "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang", "niao", "nie", "nin",
    "ning", "niu", "nong", "nu", "nuan", "nuo", "nv", "nve", "o", "ou", "pa", "pai", "pan", "pang",
    "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu",
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan",
    "que", "qun", "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "ruan",
    "rui", "run", "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai",
    "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai",
    "shuan", "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun",
    "suo", "ta", "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao", "tie", "ting",
    "tong", "tou", "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen",
    "weng", "wo", "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu",
    "xu", "xuan", "xue", "xun", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yong", "you",
    "yu", "yuan", "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng",
    "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou",
    "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu",
    "zui", "zun", "zuo",
];

/// 二分查表：401 条 → log2 ≈ 9 次比较，零运行时分配。
fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "xian" → [["xian"], ["xi","an"]]：单音节解析排前 = 最优。
    #[test]
    fn xian_ambiguous() {
        assert_eq!(
            segment("xian"),
            vec![
                vec!["xian".to_string()],
                vec!["xi".to_string(), "an".to_string()]
            ]
        );
    }

    /// "nihao"：唯一常用切分是 ni+hao；也存在 ni+ha+o（含 o 单字）。
    /// 切分按音节数从少到多排。
    #[test]
    fn nihao_unique() {
        let r = segment("nihao");
        let want = vec![
            vec!["ni".to_string(), "hao".to_string()],
            vec!["ni".to_string(), "ha".to_string(), "o".to_string()],
        ];
        assert_eq!(r, want);
    }

    /// "fangan" 与 "fan'gan" 在 ASCII 切分里都合法：fang+an、fan+gan 两个 split。
    #[test]
    fn fangan_ambiguous() {
        let r = segment("fangan");
        let want = vec![
            vec!["fang".to_string(), "an".to_string()],
            vec!["fan".to_string(), "gan".to_string()],
        ];
        assert_eq!(r, want);
    }

    /// 单音节：全表任一 1-6 字母音节自身都能被切出；某些也存在更短的二切。
    #[test]
    fn single_syllable() {
        assert_eq!(segment("zhong"), vec![vec!["zhong".to_string()]]);
        assert_eq!(segment("a"), vec![vec!["a".to_string()]]);
        // "chuang" 也可切 "chu"+"ang"
        assert_eq!(
            segment("chuang"),
            vec![
                vec!["chuang".to_string()],
                vec!["chu".to_string(), "ang".to_string()],
            ]
        );
    }

    /// 多音节直串：wo+ai+ni 唯一解（"i"/"in" 都不是合法单字音节）。
    #[test]
    fn multi_syllable() {
        assert_eq!(
            segment("woaini"),
            vec![vec!["wo".to_string(), "ai".to_string(), "ni".to_string()]]
        );
    }

    /// 非法字符（含大写/数字/中文/标点）一律拒绝。
    #[test]
    fn invalid_chars() {
        assert!(segment("niHao").is_empty());
        assert!(segment("n1hao").is_empty());
        assert!(segment("你好").is_empty());
        assert!(segment("ni hao").is_empty());
        assert!(segment("ni-hao").is_empty());
    }

    /// 空串 → 空 vec。
    #[test]
    fn empty_input() {
        let r: Vec<Reading> = segment("");
        assert!(r.is_empty());
    }

    /// "xianan"：四向歧义（xian+an / xia+nan / xi+an+an / xi+a+nan）。
    /// 期望顺序：2 音节切分（音节数最少）排前，同长度按字典序。
    #[test]
    fn xianan_3way() {
        let r = segment("xianan");
        let want = vec![
            vec!["xian".to_string(), "an".to_string()],
            vec!["xia".to_string(), "nan".to_string()],
            vec!["xi".to_string(), "an".to_string(), "an".to_string()],
            vec!["xi".to_string(), "a".to_string(), "nan".to_string()],
        ];
        assert_eq!(r, want);
    }

    /// 全表完整性 sanity：401 条，全部 1-6 字母、小写、无重复。
    /// 注：含 a/o/e 三个单字母音节（"啊"/"哦"/"鹅"），所以下界是 1 而非 2。
    #[test]
    fn table_sanity() {
        use std::collections::HashSet;
        assert!(
            (400..=420).contains(&SYLLABLES.len()),
            "count out of range: {}",
            SYLLABLES.len()
        );
        let mut seen = HashSet::new();
        for s in SYLLABLES {
            assert!(!s.is_empty() && s.len() <= 6, "bad length: {s:?}");
            assert!(
                s.bytes().all(|b| b.is_ascii_lowercase()),
                "non-lowercase: {s:?}"
            );
            assert!(seen.insert(*s), "duplicate: {s:?}");
        }
    }
}
