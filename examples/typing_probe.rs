//! 逐键延迟探针（qingjian 经验②：真实负载是增量按键，不是查整段）。
use kime_core::config::Config;
use kime_core::dict::Dict;
use kime_core::engine::{Engine, Key};
use std::time::Instant;

fn probe(label: &str, input: &str, correction: bool) {
    let db = std::env::var("HOME").unwrap() + "/.local/share/kime/dict.sqlite3";
    let dict = Dict::open(&db).unwrap();
    let mut e = Engine::new(
        dict,
        Config {
            shuangpin: None,
            correction,
            ..Config::default()
        },
    );
    let mut worst = 0f64;
    let mut worst_at = 0usize;
    let mut total = 0f64;
    for (i, c) in input.chars().enumerate() {
        let k = Key {
            ch: Some(c),
            code: 0,
            shift: false,
            ctrl: false,
            alt: false,
        };
        let t = Instant::now();
        let _ = e.key(k);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        if ms > worst {
            worst = ms;
            worst_at = i + 1;
        }
    }
    println!(
        "{label:28} correction={correction:<5} 键数 {:2}  最慢 {worst:7.3}ms@{worst_at}  平均 {:7.3}ms",
        input.chars().count(), total / input.chars().count() as f64
    );
}

fn main() {
    for (label, input) in [
        ("jintian...bucuo(27)", "jintiantianqizhendehenbucuo"),
        ("woxiangquchifan(15)", "woxiangquchifan"),
        ("nihao(5)", "nihao"),
    ] {
        probe(label, input, true);
        probe(label, input, false);
    }
}
