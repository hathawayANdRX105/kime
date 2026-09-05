# kime

个人向 Rust 拼音/双拼输入法。Linux Wayland 优先，Windows/macOS 靠平台壳后补。

## 架构

```
bin/kime/             CLI 入口（REPL、词库编译）
crates/kime-core/     引擎核心：状态机、持久化、FST/内存索引
crates/kime-pinyin/   纯拼音音节切分（404 音节表，零依赖）
crates/kime-shuangpin/双拼码表（小鹤/自然码，纯查表）
crates/platform-wayland/ input-method-v2 客户端 + layer-shell/popup 候选窗
platform-win/         （规划中）Windows TSF 壳
platform-mac/         （规划中）macOS IMKit 壳
```

## 性能指标（192 万词条实测，Ryzen/Intel 笔记本）

| 指标 | SQLite 基线 (M1-M6) | 内存排序索引 (M6.5) | FST 二进制词库 (M7) |
|---|---|---|---|
| **词库内存** | 185MB (磁盘) | **228.8MB** (堆) | **41.7MB** (mmap，按需驻留) |
| **词库载入** | 0.05s | 2.86s | **0.55s** |
| `ni'hao` 前缀查询 | 41.66ms | 0.007ms | **0.015ms** |
| `shen'me` 前缀查询 | 32.54ms | 0.305ms | **0.438ms** |
| `nh` 声母缩写查询 | ~20ms | 0.089ms | **0.0002ms** |

## 快速开始

### 1. 构建 FST 词库（一次性，将 192 万词条编译为 42MB 二进制）
```bash
cargo run --release -p kime -- build-dict \
  --in ~/.local/share/kime/dict.sqlite3 \
  --out ~/.local/share/kime/dict.bin
```

### 2. CLI 试打（REPL）
```bash
cargo run -p kime
# 或指定双拼方案
cargo run -p kime -- --shuangpin xiaohe
```

### 3. 运行性能基准测试
```bash
cargo bench --bench kime_bench
```

## 词库说明

使用 [rime-ice](https://github.com/iDvel/rime-ice)（雾凇拼音）开源词库数据（GPL-3.0）。个人本地使用合规。
