//! 小鹤双拼（flypy）码表 — 把 rime `double_pinyin_flypy` 的 algebra + preedit_format
//! 手工展开成静态查表，不再依赖运行时正则。
//!
//! 声母键：`zh→v  ch→i  sh→u`（其它声母 = 拼音首字母；零声母 'a/o/e' 双写 `aa/oo/ee`，
//! 'y/w' 前缀当软声母保留）
//!
//! 韵母键（按声母上下文分情况）：
//!   `a→a  o→o  e→e  i→i  u→u`                     单字母韵母
//!   `ü→v`                                          仅在 j/q/x/y/n/l 之后
//!   `ai→d  ei→w  ui→v  ao→c  ou→z`                 双字母复韵母
//!   `iu→q  ie→p  üe→t  er→r`                       介音在前的复韵母
//!   `an→j  en→f  in→b  un/ün→y`                    前鼻韵母
//!   `ang→h  eng→g  ing→k  ong→s`                   后鼻韵母
//!   `ian→m  uan→r  iang/uang→l`                    介音 + 前/后鼻
//!   `ia/ua→x  iao→n  iong→s`                       介音 + 后鼻/复
//!   `uo→o`                                         仅在 dt…cs 之后
//!
//! 上下文覆盖（与 rime preedit_format 对齐）：
//!   - `k` 在 g/k/h/v/ui/r/z/c/s 之后 → `uai`；否则 `ing`
//!   - `x` 在 g/k/h/v/ui/r/z/c/s 之后 → `ua`；  否则 `ia`
//!   - `r` 在 dt…cs 之后                → `uan`；否则 `er`（仅零声母）
//!   - `l` 在 j/q/x/n/l 之后            → `iang`；否则 `uang`
//!   - `s` 在 j/q/x 之后                → `iong`；否则 `ong`
//!   - `v` 在 dt…cs 之后                → `ui`； 在 j/q/x/y 之后 → `u`（ju/qu/xu/yu）
//!                                         在 n/l 之后 → `ü`（nv/lv）
//!   - `o` 在 dt…cs 之后                → `uo`；否则保持 `o`
//!
//! 零声母 `y` 一律**当声母**，韵母取本表键位：`you`=yz(ou→Z)、`yao`=yc(iao→C)、
//! `yan`=yj(an→J)、`ye`=ye。把 `y` 当介音去套别的键（yq/yn/ym/yp）是错的 ——
//! rime `double_pinyin_flypy` 的 algebra 没有那个替换。守卫见 `tests/rime_algebra_test.rs`。

/// 把 401 音节里属于本方案的「音节→键对」手工列出来。
/// 编码 = `声母键 + 韵母键`；零声母单韵母双写（如 `a→aa`）。
#[cfg(test)]
use kime_pinyin::Reading;

pub(crate) const TABLE: &[(&str, [u8; 2])] = &[
    ("a", *b"aa"),
    ("ao", *b"ac"),
    ("ai", *b"ad"),
    ("ang", *b"ah"),
    ("an", *b"aj"),
    ("ba", *b"ba"),
    ("bin", *b"bb"),
    ("bao", *b"bc"),
    ("bai", *b"bd"),
    ("ben", *b"bf"),
    ("beng", *b"bg"),
    ("bang", *b"bh"),
    ("bi", *b"bi"),
    ("ban", *b"bj"),
    ("bing", *b"bk"),
    ("bian", *b"bm"),
    ("biao", *b"bn"),
    ("bo", *b"bo"),
    ("bie", *b"bp"),
    ("bu", *b"bu"),
    ("bei", *b"bw"),
    ("ca", *b"ca"),
    ("cao", *b"cc"),
    ("cai", *b"cd"),
    ("ce", *b"ce"),
    ("cen", *b"cf"),
    ("ceng", *b"cg"),
    ("cang", *b"ch"),
    ("ci", *b"ci"),
    ("can", *b"cj"),
    ("cuo", *b"co"),
    ("cuan", *b"cr"),
    ("cong", *b"cs"),
    ("cu", *b"cu"),
    ("cui", *b"cv"),
    ("cun", *b"cy"),
    ("cou", *b"cz"),
    ("da", *b"da"),
    ("dao", *b"dc"),
    ("dai", *b"dd"),
    ("de", *b"de"),
    ("deng", *b"dg"),
    ("dang", *b"dh"),
    ("di", *b"di"),
    ("dan", *b"dj"),
    ("ding", *b"dk"),
    ("dian", *b"dm"),
    ("diao", *b"dn"),
    ("duo", *b"do"),
    ("die", *b"dp"),
    ("diu", *b"dq"),
    ("duan", *b"dr"),
    ("dong", *b"ds"),
    ("du", *b"du"),
    ("dui", *b"dv"),
    ("dei", *b"dw"),
    ("dun", *b"dy"),
    ("dou", *b"dz"),
    ("e", *b"ee"),
    ("en", *b"ef"),
    ("eng", *b"eg"),
    ("er", *b"er"),
    ("ei", *b"ew"),
    ("fa", *b"fa"),
    ("fen", *b"ff"),
    ("feng", *b"fg"),
    ("fang", *b"fh"),
    ("fan", *b"fj"),
    ("fo", *b"fo"),
    ("fu", *b"fu"),
    ("fei", *b"fw"),
    ("fou", *b"fz"),
    ("ga", *b"ga"),
    ("gao", *b"gc"),
    ("gai", *b"gd"),
    ("ge", *b"ge"),
    ("gen", *b"gf"),
    ("geng", *b"gg"),
    ("gang", *b"gh"),
    ("gan", *b"gj"),
    ("guai", *b"gk"),
    ("guang", *b"gl"),
    ("guo", *b"go"),
    ("guan", *b"gr"),
    ("gong", *b"gs"),
    ("gu", *b"gu"),
    ("gui", *b"gv"),
    ("gei", *b"gw"),
    ("gua", *b"gx"),
    ("gun", *b"gy"),
    ("gou", *b"gz"),
    ("ha", *b"ha"),
    ("hao", *b"hc"),
    ("hai", *b"hd"),
    ("he", *b"he"),
    ("hen", *b"hf"),
    ("heng", *b"hg"),
    ("hang", *b"hh"),
    ("han", *b"hj"),
    ("huai", *b"hk"),
    ("huang", *b"hl"),
    ("huo", *b"ho"),
    ("huan", *b"hr"),
    ("hong", *b"hs"),
    ("hu", *b"hu"),
    ("hui", *b"hv"),
    ("hei", *b"hw"),
    ("hua", *b"hx"),
    ("hun", *b"hy"),
    ("hou", *b"hz"),
    ("cha", *b"ia"),
    ("chao", *b"ic"),
    ("chai", *b"id"),
    ("che", *b"ie"),
    ("chen", *b"if"),
    ("cheng", *b"ig"),
    ("chang", *b"ih"),
    ("chi", *b"ii"),
    ("chan", *b"ij"),
    ("chuai", *b"ik"),
    ("chuang", *b"il"),
    ("chuo", *b"io"),
    ("chuan", *b"ir"),
    ("chong", *b"is"),
    ("chu", *b"iu"),
    ("chui", *b"iv"),
    ("chua", *b"ix"),
    ("chun", *b"iy"),
    ("chou", *b"iz"),
    ("jin", *b"jb"),
    ("ji", *b"ji"),
    ("jing", *b"jk"),
    ("jiang", *b"jl"),
    ("jian", *b"jm"),
    ("jiao", *b"jn"),
    ("jie", *b"jp"),
    ("jiu", *b"jq"),
    ("juan", *b"jr"),
    ("jiong", *b"js"),
    ("jue", *b"jt"),
    ("ju", *b"jv"),
    ("jia", *b"jx"),
    ("jun", *b"jy"),
    ("ka", *b"ka"),
    ("kao", *b"kc"),
    ("kai", *b"kd"),
    ("ke", *b"ke"),
    ("ken", *b"kf"),
    ("keng", *b"kg"),
    ("kang", *b"kh"),
    ("kan", *b"kj"),
    ("kuai", *b"kk"),
    ("kuang", *b"kl"),
    ("kuo", *b"ko"),
    ("kuan", *b"kr"),
    ("kong", *b"ks"),
    ("ku", *b"ku"),
    ("kui", *b"kv"),
    ("kua", *b"kx"),
    ("kun", *b"ky"),
    ("kou", *b"kz"),
    ("la", *b"la"),
    ("lin", *b"lb"),
    ("lao", *b"lc"),
    ("lai", *b"ld"),
    ("le", *b"le"),
    ("leng", *b"lg"),
    ("lang", *b"lh"),
    ("li", *b"li"),
    ("lan", *b"lj"),
    ("ling", *b"lk"),
    ("liang", *b"ll"),
    ("lian", *b"lm"),
    ("liao", *b"ln"),
    ("luo", *b"lo"),
    ("lie", *b"lp"),
    ("liu", *b"lq"),
    ("luan", *b"lr"),
    ("long", *b"ls"),
    ("lve", *b"lt"),
    ("lu", *b"lu"),
    ("lv", *b"lv"),
    ("lei", *b"lw"),
    ("lia", *b"lx"),
    ("lun", *b"ly"),
    ("lou", *b"lz"),
    ("ma", *b"ma"),
    ("min", *b"mb"),
    ("mao", *b"mc"),
    ("mai", *b"md"),
    ("me", *b"me"),
    ("men", *b"mf"),
    ("meng", *b"mg"),
    ("mang", *b"mh"),
    ("mi", *b"mi"),
    ("man", *b"mj"),
    ("ming", *b"mk"),
    ("mian", *b"mm"),
    ("miao", *b"mn"),
    ("mo", *b"mo"),
    ("mie", *b"mp"),
    ("miu", *b"mq"),
    ("mu", *b"mu"),
    ("mei", *b"mw"),
    ("mou", *b"mz"),
    ("na", *b"na"),
    ("nin", *b"nb"),
    ("nao", *b"nc"),
    ("nai", *b"nd"),
    ("ne", *b"ne"),
    ("nen", *b"nf"),
    ("neng", *b"ng"),
    ("nang", *b"nh"),
    ("ni", *b"ni"),
    ("nan", *b"nj"),
    ("ning", *b"nk"),
    ("niang", *b"nl"),
    ("nian", *b"nm"),
    ("niao", *b"nn"),
    ("nuo", *b"no"),
    ("nie", *b"np"),
    ("niu", *b"nq"),
    ("nuan", *b"nr"),
    ("nong", *b"ns"),
    ("nve", *b"nt"),
    ("nu", *b"nu"),
    ("nv", *b"nv"),
    ("nei", *b"nw"),
    ("o", *b"oo"),
    ("ou", *b"oz"),
    ("pa", *b"pa"),
    ("pin", *b"pb"),
    ("pao", *b"pc"),
    ("pai", *b"pd"),
    ("pen", *b"pf"),
    ("peng", *b"pg"),
    ("pang", *b"ph"),
    ("pi", *b"pi"),
    ("pan", *b"pj"),
    ("ping", *b"pk"),
    ("pian", *b"pm"),
    ("piao", *b"pn"),
    ("po", *b"po"),
    ("pie", *b"pp"),
    ("pu", *b"pu"),
    ("pei", *b"pw"),
    ("pou", *b"pz"),
    ("qin", *b"qb"),
    ("qi", *b"qi"),
    ("qing", *b"qk"),
    ("qiang", *b"ql"),
    ("qian", *b"qm"),
    ("qiao", *b"qn"),
    ("qie", *b"qp"),
    ("qiu", *b"qq"),
    ("quan", *b"qr"),
    ("qiong", *b"qs"),
    ("que", *b"qt"),
    ("qu", *b"qv"),
    ("qia", *b"qx"),
    ("qun", *b"qy"),
    ("rao", *b"rc"),
    ("re", *b"re"),
    ("ren", *b"rf"),
    ("reng", *b"rg"),
    ("rang", *b"rh"),
    ("ri", *b"ri"),
    ("ran", *b"rj"),
    ("ruo", *b"ro"),
    ("ruan", *b"rr"),
    ("rong", *b"rs"),
    ("ru", *b"ru"),
    ("rui", *b"rv"),
    ("run", *b"ry"),
    ("rou", *b"rz"),
    ("sa", *b"sa"),
    ("sao", *b"sc"),
    ("sai", *b"sd"),
    ("se", *b"se"),
    ("sen", *b"sf"),
    ("seng", *b"sg"),
    ("sang", *b"sh"),
    ("si", *b"si"),
    ("san", *b"sj"),
    ("suo", *b"so"),
    ("suan", *b"sr"),
    ("song", *b"ss"),
    ("su", *b"su"),
    ("sui", *b"sv"),
    ("sun", *b"sy"),
    ("sou", *b"sz"),
    ("ta", *b"ta"),
    ("tao", *b"tc"),
    ("tai", *b"td"),
    ("te", *b"te"),
    ("teng", *b"tg"),
    ("tang", *b"th"),
    ("ti", *b"ti"),
    ("tan", *b"tj"),
    ("ting", *b"tk"),
    ("tian", *b"tm"),
    ("tiao", *b"tn"),
    ("tuo", *b"to"),
    ("tie", *b"tp"),
    ("tuan", *b"tr"),
    ("tong", *b"ts"),
    ("tu", *b"tu"),
    ("tui", *b"tv"),
    ("tun", *b"ty"),
    ("tou", *b"tz"),
    ("sha", *b"ua"),
    ("shao", *b"uc"),
    ("shai", *b"ud"),
    ("she", *b"ue"),
    ("shen", *b"uf"),
    ("sheng", *b"ug"),
    ("shang", *b"uh"),
    ("shi", *b"ui"),
    ("shan", *b"uj"),
    ("shuai", *b"uk"),
    ("shuang", *b"ul"),
    ("shuo", *b"uo"),
    ("shuan", *b"ur"),
    ("shu", *b"uu"),
    ("shui", *b"uv"),
    ("shei", *b"uw"),
    ("shua", *b"ux"),
    ("shun", *b"uy"),
    ("shou", *b"uz"),
    ("zha", *b"va"),
    ("zhao", *b"vc"),
    ("zhai", *b"vd"),
    ("zhe", *b"ve"),
    ("zhen", *b"vf"),
    ("zheng", *b"vg"),
    ("zhang", *b"vh"),
    ("zhi", *b"vi"),
    ("zhan", *b"vj"),
    ("zhuai", *b"vk"),
    ("zhuang", *b"vl"),
    ("zhuo", *b"vo"),
    ("zhuan", *b"vr"),
    ("zhong", *b"vs"),
    ("zhu", *b"vu"),
    ("zhui", *b"vv"),
    ("zhei", *b"vw"),
    ("zhua", *b"vx"),
    ("zhun", *b"vy"),
    ("zhou", *b"vz"),
    ("wa", *b"wa"),
    ("wai", *b"wd"),
    ("wen", *b"wf"),
    ("weng", *b"wg"),
    ("wang", *b"wh"),
    ("wan", *b"wj"),
    ("wo", *b"wo"),
    ("wu", *b"wu"),
    ("wei", *b"ww"),
    ("xin", *b"xb"),
    ("xi", *b"xi"),
    ("xing", *b"xk"),
    ("xiang", *b"xl"),
    ("xian", *b"xm"),
    ("xiao", *b"xn"),
    ("xie", *b"xp"),
    ("xiu", *b"xq"),
    ("xuan", *b"xr"),
    ("xiong", *b"xs"),
    ("xue", *b"xt"),
    ("xu", *b"xv"),
    ("xia", *b"xx"),
    ("xun", *b"xy"),
    ("yin", *b"yb"),
    ("yao", *b"yc"),
    ("ye", *b"ye"),
    ("yang", *b"yh"),
    ("yi", *b"yi"),
    ("yan", *b"yj"),
    ("ying", *b"yk"),
    ("yuan", *b"yr"),
    ("yong", *b"ys"),
    ("yue", *b"yt"),
    ("yu", *b"yv"),
    ("yun", *b"yy"),
    ("you", *b"yz"),
    ("za", *b"za"),
    ("zao", *b"zc"),
    ("zai", *b"zd"),
    ("ze", *b"ze"),
    ("zen", *b"zf"),
    ("zeng", *b"zg"),
    ("zang", *b"zh"),
    ("zi", *b"zi"),
    ("zan", *b"zj"),
    ("zuo", *b"zo"),
    ("zong", *b"zs"),
    ("zu", *b"zu"),
    ("zui", *b"zv"),
    ("zei", *b"zw"),
    ("zun", *b"zy"),
    ("zou", *b"zz"),
];

#[cfg(test)]
/// 401 音节 → 2 字节键。返回 `None` 表示非本方案可表示音节或编码冲突。
pub(crate) fn encode(syll: &str) -> Option<[u8; 2]> {
    // ponytail: linear scan over 401 entries is faster than the binary search
    // we'd need if we kept two sort orders; the hot path here is decode, not encode.
    TABLE.iter().find(|&&(k, _)| k == syll).map(|&(_, v)| v)
}

/// 反向查表：键对 → 音节。`(a, b)` 必须都是小写 a-z；其它（含 `Err` 哨兵）→ `None`。
pub(crate) fn decode(a: u8, b: u8) -> Option<&'static str> {
    if !a.is_ascii_lowercase() || !b.is_ascii_lowercase() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = TABLE.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (syl, key) = TABLE[mid];
        match key.cmp(&[a, b]) {
            std::cmp::Ordering::Equal => return Some(syl),
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    None
}

#[cfg(test)]
/// 把整串双拼键切成音节序列。长度非偶数 / 任何一对不合法 → `Err`。
pub(crate) fn to_syllables(keys: &str) -> Result<Reading, String> {
    if keys.len() % 2 != 0 {
        return Err(format!("odd key length: {}", keys.len()));
    }
    let bytes = keys.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_lowercase()) {
        return Err("non a-z key".into());
    }
    let mut out = Reading::with_capacity(keys.len() / 2);
    for pair in bytes.chunks_exact(2) {
        match decode(pair[0], pair[1]) {
            Some(s) => out.push(s.to_string()),
            None => {
                return Err(format!(
                    "bad key pair: {}{}",
                    pair[0] as char, pair[1] as char
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全拼音节表 round-trip：编码 → 解码 必须等于原音节。
    /// ponytail: 唯一允许的非平凡自检，覆盖 401 音节 × 双拼方案全表。
    #[test]
    fn round_trip_all() {
        for &(syl, key) in TABLE {
            assert_eq!(encode(syl), Some(key), "encode({syl})");
            assert_eq!(decode(key[0], key[1]), Some(syl), "decode({key:?})");
            // 跨表查：encode→decode→encode 应该稳定
            let decoded = decode(key[0], key[1]).unwrap();
            let re_encoded = encode(decoded).unwrap();
            assert_eq!(re_encoded, key, "encode∘decode({syl}) round-trip");
        }
    }

    /// 解码唯一性：不同音节不共享同一键对。
    #[test]
    fn decode_unique() {
        for i in 0..TABLE.len() {
            for j in (i + 1)..TABLE.len() {
                assert_ne!(TABLE[i].1, TABLE[j].1, "{} and {}", TABLE[i].0, TABLE[j].0);
            }
        }
    }

    /// 解码密度：26×26 全表扫描，命中数应 = TABLE.len()。
    #[test]
    fn decode_density() {
        let mut hits = 0;
        for a in b'a'..=b'z' {
            for b in b'a'..=b'z' {
                if decode(a, b).is_some() {
                    hits += 1;
                }
            }
        }
        assert_eq!(hits, TABLE.len());
    }

    /// 任务里给的典型示例：标准小鹤 `nihc` → `["ni","hao"]`。
    /// ponytail: 任务原文写成 `nihk`，那是 typo（k 是 ing 不是 ao）。
    #[test]
    fn nihao_xiaohe() {
        assert_eq!(to_syllables("nihc").unwrap(), vec!["ni", "hao"]);
    }

    #[test]
    fn odd_length_err() {
        // 单字符 / 3 字符：非偶数 → Err；空串 0%2=0 视为「无输入」不报错。
        assert!(to_syllables("n").is_err());
        assert!(to_syllables("nih").is_err());
    }

    #[test]
    fn invalid_key_err() {
        // "ab" 不在表里（a 后面跟 b 不是任何合法音节键对）
        assert!(to_syllables("ab").is_err());
        // 大写不允许
        assert!(to_syllables("Ni").is_err());
    }
}
