use std::time::Instant;
fn main() {
    let t0 = Instant::now();
    let dict =
        kime_core::dict::Dict::open("/home/hathaway/.local/share/kime/dict.sqlite3").unwrap();
    println!("open+load 1.9M: {:.2?}s", t0.elapsed().as_secs_f64());
    for q in [["ni", "hao"], ["shen", "me"], ["ni", "ha"], ["g", "x"]] {
        let t = Instant::now();
        let n = 100;
        for _ in 0..n {
            let _ = dict.lookup_prefix(&[q[0].to_string()], q[1], 10).unwrap();
        }
        println!(
            "lookup_prefix {:?}: {:.3}ms/op",
            q,
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
    let t = Instant::now();
    for _ in 0..100 {
        let _ = dict.lookup_abbrev("nh", 10).unwrap();
    }
    println!(
        "lookup_abbrev nh: {:.3}ms/op",
        t.elapsed().as_secs_f64() * 1000.0 / 100.0
    );
}
