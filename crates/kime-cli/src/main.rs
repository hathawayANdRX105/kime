//! kime 命令行入口 — M1/M2 的可运行验收。
//!
//! 契约：stdin 每行一个输入串（全拼字母或双拼键序），打印 top-10 候选；
//! 词库打不开 → exit 非 0。这个 REPL 循环同时是 Engine 的最小壳原型。

fn main() {
    todo!("M1: Config::default → Dict::open → 逐行 segment(或双拼翻译) → lookup(10) → 打印")
}
