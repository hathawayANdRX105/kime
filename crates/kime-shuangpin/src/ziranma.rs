//! 自然码双拼码表 — 把 rime `double_pinyin` 的 algebra + preedit_format
//! 手工展开成静态查表，不再依赖运行时正则。
//!
//! 声母键：`zh→v  ch→i  sh→u`（与小鹤一致；其它声母 = 拼音首字母）
//!
//! 韵母键（与小鹤不同）：
//!   `a→a  o→o  e→e  i→i  u→u`                     单字母韵母
//!   `ü→v`                                          仅在 j/q/x/y/n/l 之后
//!   `ai→l  ei→z  ui→v  ao→k  ou→b`                 双字母复韵母
//!   `iu→q  ie→x  üe→t  er→r`                       介音在前的复韵母
//!   `an→j  en→f  in→n  un/ün→p`                    前鼻韵母
//!   `ang→h  eng→g  ing→y  ong→s`                   后鼻韵母
//!   `ian→m  uan→r  iang/uang→d`                    介音 + 前/后鼻
//!   `ia/ua→w  iao→c  iong→s`                       介音 + 后鼻/复
//!   `uo→o`                                         仅在 dt…cs 之后
//!
//! 上下文覆盖：
//!   - `y` 在 g/k/h/v/ui/r/z/c/s 之后 → `uai`；否则 `ing`
//!   - `w` 在 g/k/h/v/ui/r/z/c/s 之后 → `ua`；  否则 `ia`
//!   - `r` 在 dt…cs 之后                → `uan`；否则 `er`（仅零声母）
//!   - `d` 在 j/q/x/l 之后              → `iang`；否则 `uang`
//!   - `s` 在 j/q/x 之后                → `iong`；否则 `ong`
//!   - `v` 在 dt…cs 之后                → `ui`； 在 j/q/x/y 之后 → `u`（ju/qu/xu/yu）
//!                                         在 n/l 之后 → `ü`（nv/lv）
//!   - `o` 在 dt…cs 之后                → `uo`；否则保持 `o`

#[cfg(test)]
use kime_pinyin::Reading;

pub(crate) const TABLE: &[(&str, [u8; 2])] = &[
    ("a", *b"aa"),
    ("ang", *b"ah"),
    ("an", *b"aj"),
    ("ao", *b"ak"),
    ("ai", *b"al"),
    ("ba", *b"ba"),
    ("biao", *b"bc"),
    ("ben", *b"bf"),
    ("beng", *b"bg"),
    ("bang", *b"bh"),
    ("bi", *b"bi"),
    ("ban", *b"bj"),
    ("bao", *b"bk"),
    ("bai", *b"bl"),
    ("bian", *b"bm"),
    ("bin", *b"bn"),
    ("bo", *b"bo"),
    ("bu", *b"bu"),
    ("bie", *b"bx"),
    ("bing", *b"by"),
    ("bei", *b"bz"),
    ("ca", *b"ca"),
    ("cou", *b"cb"),
    ("ce", *b"ce"),
    ("cen", *b"cf"),
    ("ceng", *b"cg"),
    ("cang", *b"ch"),
    ("ci", *b"ci"),
    ("can", *b"cj"),
    ("cao", *b"ck"),
    ("cai", *b"cl"),
    ("cuo", *b"co"),
    ("cun", *b"cp"),
    ("cuan", *b"cr"),
    ("cong", *b"cs"),
    ("cu", *b"cu"),
    ("cui", *b"cv"),
    ("da", *b"da"),
    ("dou", *b"db"),
    ("diao", *b"dc"),
    ("de", *b"de"),
    ("deng", *b"dg"),
    ("dang", *b"dh"),
    ("di", *b"di"),
    ("dan", *b"dj"),
    ("dao", *b"dk"),
    ("dai", *b"dl"),
    ("dian", *b"dm"),
    ("duo", *b"do"),
    ("dun", *b"dp"),
    ("diu", *b"dq"),
    ("duan", *b"dr"),
    ("dong", *b"ds"),
    ("du", *b"du"),
    ("dui", *b"dv"),
    ("die", *b"dx"),
    ("ding", *b"dy"),
    ("dei", *b"dz"),
    ("e", *b"ee"),
    ("en", *b"ef"),
    ("eng", *b"eg"),
    ("er", *b"er"),
    ("ei", *b"ez"),
    ("fa", *b"fa"),
    ("fou", *b"fb"),
    ("fen", *b"ff"),
    ("feng", *b"fg"),
    ("fang", *b"fh"),
    ("fan", *b"fj"),
    ("fo", *b"fo"),
    ("fu", *b"fu"),
    ("fei", *b"fz"),
    ("ga", *b"ga"),
    ("gou", *b"gb"),
    ("guang", *b"gd"),
    ("ge", *b"ge"),
    ("gen", *b"gf"),
    ("geng", *b"gg"),
    ("gang", *b"gh"),
    ("gan", *b"gj"),
    ("gao", *b"gk"),
    ("gai", *b"gl"),
    ("guo", *b"go"),
    ("gun", *b"gp"),
    ("guan", *b"gr"),
    ("gong", *b"gs"),
    ("gu", *b"gu"),
    ("gui", *b"gv"),
    ("gua", *b"gw"),
    ("guai", *b"gy"),
    ("gei", *b"gz"),
    ("ha", *b"ha"),
    ("hou", *b"hb"),
    ("huang", *b"hd"),
    ("he", *b"he"),
    ("hen", *b"hf"),
    ("heng", *b"hg"),
    ("hang", *b"hh"),
    ("han", *b"hj"),
    ("hao", *b"hk"),
    ("hai", *b"hl"),
    ("huo", *b"ho"),
    ("hun", *b"hp"),
    ("huan", *b"hr"),
    ("hong", *b"hs"),
    ("hu", *b"hu"),
    ("hui", *b"hv"),
    ("hua", *b"hw"),
    ("huai", *b"hy"),
    ("hei", *b"hz"),
    ("cha", *b"ia"),
    ("chou", *b"ib"),
    ("chuang", *b"id"),
    ("che", *b"ie"),
    ("chen", *b"if"),
    ("cheng", *b"ig"),
    ("chang", *b"ih"),
    ("chi", *b"ii"),
    ("chan", *b"ij"),
    ("chao", *b"ik"),
    ("chai", *b"il"),
    ("chuo", *b"io"),
    ("chun", *b"ip"),
    ("chuan", *b"ir"),
    ("chong", *b"is"),
    ("chu", *b"iu"),
    ("chui", *b"iv"),
    ("chua", *b"iw"),
    ("chuai", *b"iy"),
    ("jiao", *b"jc"),
    ("jiang", *b"jd"),
    ("ji", *b"ji"),
    ("jian", *b"jm"),
    ("jin", *b"jn"),
    ("jun", *b"jp"),
    ("jiu", *b"jq"),
    ("juan", *b"jr"),
    ("jiong", *b"js"),
    ("jue", *b"jt"),
    ("ju", *b"jv"),
    ("jia", *b"jw"),
    ("jie", *b"jx"),
    ("jing", *b"jy"),
    ("ka", *b"ka"),
    ("kou", *b"kb"),
    ("kuang", *b"kd"),
    ("ke", *b"ke"),
    ("ken", *b"kf"),
    ("keng", *b"kg"),
    ("kang", *b"kh"),
    ("kan", *b"kj"),
    ("kao", *b"kk"),
    ("kai", *b"kl"),
    ("kuo", *b"ko"),
    ("kun", *b"kp"),
    ("kuan", *b"kr"),
    ("kong", *b"ks"),
    ("ku", *b"ku"),
    ("kui", *b"kv"),
    ("kua", *b"kw"),
    ("kuai", *b"ky"),
    ("la", *b"la"),
    ("lou", *b"lb"),
    ("liao", *b"lc"),
    ("liang", *b"ld"),
    ("le", *b"le"),
    ("leng", *b"lg"),
    ("lang", *b"lh"),
    ("li", *b"li"),
    ("lan", *b"lj"),
    ("lao", *b"lk"),
    ("lai", *b"ll"),
    ("lian", *b"lm"),
    ("lin", *b"ln"),
    ("luo", *b"lo"),
    ("lun", *b"lp"),
    ("liu", *b"lq"),
    ("luan", *b"lr"),
    ("long", *b"ls"),
    ("lve", *b"lt"),
    ("lu", *b"lu"),
    ("lv", *b"lv"),
    ("lia", *b"lw"),
    ("lie", *b"lx"),
    ("ling", *b"ly"),
    ("lei", *b"lz"),
    ("ma", *b"ma"),
    ("mou", *b"mb"),
    ("miao", *b"mc"),
    ("me", *b"me"),
    ("men", *b"mf"),
    ("meng", *b"mg"),
    ("mang", *b"mh"),
    ("mi", *b"mi"),
    ("man", *b"mj"),
    ("mao", *b"mk"),
    ("mai", *b"ml"),
    ("mian", *b"mm"),
    ("min", *b"mn"),
    ("mo", *b"mo"),
    ("miu", *b"mq"),
    ("mu", *b"mu"),
    ("mie", *b"mx"),
    ("ming", *b"my"),
    ("mei", *b"mz"),
    ("na", *b"na"),
    ("niao", *b"nc"),
    ("niang", *b"nd"),
    ("ne", *b"ne"),
    ("nen", *b"nf"),
    ("neng", *b"ng"),
    ("nang", *b"nh"),
    ("ni", *b"ni"),
    ("nan", *b"nj"),
    ("nao", *b"nk"),
    ("nai", *b"nl"),
    ("nian", *b"nm"),
    ("nin", *b"nn"),
    ("nuo", *b"no"),
    ("niu", *b"nq"),
    ("nuan", *b"nr"),
    ("nong", *b"ns"),
    ("nve", *b"nt"),
    ("nu", *b"nu"),
    ("nv", *b"nv"),
    ("nie", *b"nx"),
    ("ning", *b"ny"),
    ("nei", *b"nz"),
    ("ou", *b"ob"),
    ("o", *b"oo"),
    ("pa", *b"pa"),
    ("pou", *b"pb"),
    ("piao", *b"pc"),
    ("pen", *b"pf"),
    ("peng", *b"pg"),
    ("pang", *b"ph"),
    ("pi", *b"pi"),
    ("pan", *b"pj"),
    ("pao", *b"pk"),
    ("pai", *b"pl"),
    ("pian", *b"pm"),
    ("pin", *b"pn"),
    ("po", *b"po"),
    ("pu", *b"pu"),
    ("pie", *b"px"),
    ("ping", *b"py"),
    ("pei", *b"pz"),
    ("qiao", *b"qc"),
    ("qiang", *b"qd"),
    ("qi", *b"qi"),
    ("qian", *b"qm"),
    ("qin", *b"qn"),
    ("qun", *b"qp"),
    ("qiu", *b"qq"),
    ("quan", *b"qr"),
    ("qiong", *b"qs"),
    ("que", *b"qt"),
    ("qu", *b"qv"),
    ("qia", *b"qw"),
    ("qie", *b"qx"),
    ("qing", *b"qy"),
    ("rou", *b"rb"),
    ("re", *b"re"),
    ("ren", *b"rf"),
    ("reng", *b"rg"),
    ("rang", *b"rh"),
    ("ri", *b"ri"),
    ("ran", *b"rj"),
    ("rao", *b"rk"),
    ("ruo", *b"ro"),
    ("run", *b"rp"),
    ("ruan", *b"rr"),
    ("rong", *b"rs"),
    ("ru", *b"ru"),
    ("rui", *b"rv"),
    ("sa", *b"sa"),
    ("sou", *b"sb"),
    ("se", *b"se"),
    ("sen", *b"sf"),
    ("seng", *b"sg"),
    ("sang", *b"sh"),
    ("si", *b"si"),
    ("san", *b"sj"),
    ("sao", *b"sk"),
    ("sai", *b"sl"),
    ("suo", *b"so"),
    ("sun", *b"sp"),
    ("suan", *b"sr"),
    ("song", *b"ss"),
    ("su", *b"su"),
    ("sui", *b"sv"),
    ("ta", *b"ta"),
    ("tou", *b"tb"),
    ("tiao", *b"tc"),
    ("te", *b"te"),
    ("teng", *b"tg"),
    ("tang", *b"th"),
    ("ti", *b"ti"),
    ("tan", *b"tj"),
    ("tao", *b"tk"),
    ("tai", *b"tl"),
    ("tian", *b"tm"),
    ("tuo", *b"to"),
    ("tun", *b"tp"),
    ("tuan", *b"tr"),
    ("tong", *b"ts"),
    ("tu", *b"tu"),
    ("tui", *b"tv"),
    ("tie", *b"tx"),
    ("ting", *b"ty"),
    ("sha", *b"ua"),
    ("shou", *b"ub"),
    ("shuang", *b"ud"),
    ("she", *b"ue"),
    ("shen", *b"uf"),
    ("sheng", *b"ug"),
    ("shang", *b"uh"),
    ("shi", *b"ui"),
    ("shan", *b"uj"),
    ("shao", *b"uk"),
    ("shai", *b"ul"),
    ("shuo", *b"uo"),
    ("shun", *b"up"),
    ("shuan", *b"ur"),
    ("shu", *b"uu"),
    ("shui", *b"uv"),
    ("shua", *b"uw"),
    ("shuai", *b"uy"),
    ("shei", *b"uz"),
    ("zha", *b"va"),
    ("zhou", *b"vb"),
    ("zhuang", *b"vd"),
    ("zhe", *b"ve"),
    ("zhen", *b"vf"),
    ("zheng", *b"vg"),
    ("zhang", *b"vh"),
    ("zhi", *b"vi"),
    ("zhan", *b"vj"),
    ("zhao", *b"vk"),
    ("zhai", *b"vl"),
    ("zhuo", *b"vo"),
    ("zhun", *b"vp"),
    ("zhuan", *b"vr"),
    ("zhong", *b"vs"),
    ("zhu", *b"vu"),
    ("zhui", *b"vv"),
    ("zhua", *b"vw"),
    ("zhuai", *b"vy"),
    ("zhei", *b"vz"),
    ("wa", *b"wa"),
    ("wen", *b"wf"),
    ("weng", *b"wg"),
    ("wang", *b"wh"),
    ("wan", *b"wj"),
    ("wai", *b"wl"),
    ("wo", *b"wo"),
    ("wu", *b"wu"),
    ("wei", *b"wz"),
    ("xiao", *b"xc"),
    ("xiang", *b"xd"),
    ("xi", *b"xi"),
    ("xian", *b"xm"),
    ("xin", *b"xn"),
    ("xun", *b"xp"),
    ("xiu", *b"xq"),
    ("xuan", *b"xr"),
    ("xiong", *b"xs"),
    ("xue", *b"xt"),
    ("xu", *b"xv"),
    ("xia", *b"xw"),
    ("xie", *b"xx"),
    ("xing", *b"xy"),
    ("yao", *b"yc"),
    ("yang", *b"yh"),
    ("yi", *b"yi"),
    ("yan", *b"ym"),
    ("yin", *b"yn"),
    ("yun", *b"yp"),
    ("you", *b"yq"),
    ("yuan", *b"yr"),
    ("yong", *b"ys"),
    ("yue", *b"yt"),
    ("yu", *b"yv"),
    ("ye", *b"yx"),
    ("ying", *b"yy"),
    ("za", *b"za"),
    ("zou", *b"zb"),
    ("ze", *b"ze"),
    ("zen", *b"zf"),
    ("zeng", *b"zg"),
    ("zang", *b"zh"),
    ("zi", *b"zi"),
    ("zan", *b"zj"),
    ("zao", *b"zk"),
    ("zai", *b"zl"),
    ("zuo", *b"zo"),
    ("zun", *b"zp"),
    ("zong", *b"zs"),
    ("zu", *b"zu"),
    ("zui", *b"zv"),
    ("zei", *b"zz"),
];

#[cfg(test)]
/// 401 音节 → 2 字节键。返回 `None` 表示非本方案可表示音节或编码冲突。
pub(crate) fn encode(syll: &str) -> Option<[u8; 2]> {
    // ponytail: linear scan over 401 entries is faster than the binary search
    // we'd need if we kept two sort orders; the hot path here is decode, not encode.
    TABLE.iter().find(|&&(k, _)| k == syll).map(|&(_, v)| v)
}

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

    #[test]
    fn round_trip_all() {
        for &(syl, key) in TABLE {
            assert_eq!(encode(syl), Some(key), "encode({syl})");
            assert_eq!(decode(key[0], key[1]), Some(syl), "decode({key:?})");
            let decoded = decode(key[0], key[1]).unwrap();
            let re_encoded = encode(decoded).unwrap();
            assert_eq!(re_encoded, key, "encode∘decode({syl}) round-trip");
        }
    }

    #[test]
    fn decode_unique() {
        for i in 0..TABLE.len() {
            for j in (i + 1)..TABLE.len() {
                assert_ne!(TABLE[i].1, TABLE[j].1, "{} and {}", TABLE[i].0, TABLE[j].0);
            }
        }
    }

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

    /// 自然码里 "hao" 的末键是 k（不是 c），所以 "nihao" = "nihk"。
    /// ponytail: 任务原例 `nihk→[ni,hao]` 实际上配的是自然码，小鹤是 "nihc"。
    #[test]
    fn nihao_ziranma() {
        assert_eq!(to_syllables("nihk").unwrap(), vec!["ni", "hao"]);
    }

    #[test]
    fn odd_length_err() {
        // 单字符 / 3 字符：非偶数 → Err；空串 0%2=0 视为「无输入」不报错。
        assert!(to_syllables("n").is_err());
        assert!(to_syllables("nih").is_err());
    }

    #[test]
    fn invalid_key_err() {
        // "ab" 不在表里
        assert!(to_syllables("ab").is_err());
        // 大写不允许
        assert!(to_syllables("Ni").is_err());
    }
}
