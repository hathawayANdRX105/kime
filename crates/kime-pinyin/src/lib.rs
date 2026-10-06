//! 拼音音节切分 — 纯逻辑，零依赖。
//!
//! 契约：ascii 小写字母串 → 所有合法切分，DFS 展开序（见 [`segment`]）。
//! "xian" → [["xian"], ["xi","an"]]；"nihao" → [["ni","hao"], ["ni","ha","o"]]；
//! 空串 / 非 a-z 字符 / 无法切分 → 空 vec。
//!
//! 只认标准普通话无调音节表（401 条，含 a/o/e 单字）；声母缩写（nh）由
//! dict 层的 abbrev 列承担，不在此处。

/// 一次切分：音节序列，如 ["ni","hao"]
pub type Reading = Vec<String>;

/// 全部合法切分，DFS 展开序：每步先尝试更长的音节前缀，因此更长首音节、通常也是
/// 音节数更少的切分排在前面；**同音节数之间没有字典序或其他二次排序**。
/// 消费者要覆盖歧义必须遍历全部切分，不得押注首条（引擎侧正因此修过「蛋糕」bug）。
/// 算法：DFS 从左到右探索，每步尝试 6→1 字母的音节前缀。空串/非法字符/无解 → 空 vec。
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

/// 无调音节表（416 条；按字典序排好）。
///
/// 全集依据：线上词库 dict.sqlite3 phrase.pinyin 的全部去重音节（416）——
/// 词库里有的读音必须能切出来，否则该词在全拼下打不到。逐条核对为
/// rime-luna-pinyin.dict.yaml 音节集（424）的子集：新增 15 条均为词典在册音节
/// （ya 丫、yo 哟、lo 啰、den 扽、nou、nun、kei、cei、tei、dia、nia、rua、zuan、
/// fiao 覅、biang），未收录 luna 独有的 eh/fong/lvan/sei/wong/yai（词库无字）。
pub const SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biang", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao",
    "ce", "cei", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng",
    "chi", "chong", "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo",
    "ci", "cong", "cou", "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao",
    "de", "dei", "den", "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou",
    "du", "duan", "dui", "dun", "duo", "e", "ei", "en", "eng", "er", "fa", "fan", "fang", "fei",
    "fen", "feng", "fiao", "fo", "fou", "fu", "ga", "gai", "gan", "gang", "gao", "ge", "gei",
    "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo", "ha",
    "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai",
    "huan", "huang", "hui", "hun", "huo", "ji", "jia", "jian", "jiang", "jiao", "jie", "jin",
    "jing", "jiong", "jiu", "ju", "juan", "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke",
    "kei", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao",
    "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie",
    "min", "ming", "miu", "mo", "mou", "mu", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen",
    "neng", "ni", "nia", "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu",
    "nuan", "nun", "nuo", "nv", "nve", "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen",
    "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia", "qian",
    "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun", "ran",
    "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "rua", "ruan", "rui", "run",
    "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang",
    "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan",
    "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo", "ta",
    "tai", "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting", "tong",
    "tou", "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen", "weng",
    "wo", "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu",
    "xuan", "xue", "xun", "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong",
    "you", "yu", "yuan", "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen",
    "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong",
    "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou",
    "zu", "zuan", "zui", "zun", "zuo",
];

/// 二分查表：416 条 → log2 ≈ 9 次比较，零运行时分配。
pub fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

/// 一维可达性 DP：判断按键串能否被完整切分成合法音节（不产出任何切分、零分配）。
///
/// 用途：纠错变体的廉价预筛——编辑距离 1 的变体绝大多数仍是非法串，先用它挡掉，
/// 幸存的极少数才进完整 segment()（qingjian 同款教训：纠错枚举不预筛时逐变体跑
/// 切分 DP，长句每键几十毫秒）。
pub fn is_fully_segmentable(letters: &str) -> bool {
    if letters.is_empty() {
        return false;
    }
    let b = letters.as_bytes();
    let n = b.len();
    // reach[i] = 前 i 个字节可切分
    let mut reach = [false; 33]; // kime 音节最长 6 字母；保守 33 覆盖任意输入前缀长度
    if n >= 33 {
        // 超长输入：用滚动窗口（窗口大小 = 最长音节长度）
        let max_syl = 6usize;
        let mut window = vec![false; n + 1];
        window[0] = true;
        for i in 0..n {
            if !window[i] {
                continue;
            }
            for l in 1..=max_syl.min(n - i) {
                if is_syllable_bytes(&b[i..i + l]) {
                    window[i + l] = true;
                }
            }
        }
        return window[n];
    }
    reach[0] = true;
    for i in 0..n {
        if !reach[i] {
            continue;
        }
        for l in 1..=(n - i).min(6) {
            if is_syllable_bytes(&b[i..i + l]) {
                reach[i + l] = true;
            }
        }
    }
    reach[n]
}

fn is_syllable_bytes(b: &[u8]) -> bool {
    std::str::from_utf8(b).ok().is_some_and(is_syllable)
}
