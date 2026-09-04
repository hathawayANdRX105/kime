# kime

个人向 Rust 拼音/双拼输入法。Linux Wayland 优先，Windows/macOS 靠平台壳后补。

## 范围（刻意收窄）

- 引擎：只做拼音 + 双拼（小鹤/自然码表），不做五笔等其他方案
- 平台：先 Linux Wayland（input-method-v2）；X11/XIM、TSF、IMKit 等需要时再说
- 存储：SQLite——词库、用户词、词频学习、个人习惯映射，全部落一个文件
- 特色：AI 预测候选（异步第二梯队，不挡 ~20ms 的本地候选主路）

## 架构

```
kime-core/      引擎：音节切分、双拼码表、候选排序、学习。零平台依赖
kime-wayland/   input-method-v2 前端 + layer-shell 候选窗
platform-win/   （以后）TSF 壳，参考 Weasel
platform-mac/   （以后）IMKit 壳，参考 Squirrel
```

规则：core 不碰显示/IPC；壳只做「按键进、候选出」。

## 词库

打算用雾凇拼音（rime-ice）开源词库数据，发布前核 license。
