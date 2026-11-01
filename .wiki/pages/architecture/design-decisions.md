---
id: architecture/design-decisions
title: 设计决策与实测依据
status: verified
generated_at: 2026-11-01T03:28:05Z
commit: a1601d77cd134732efe85d81a5f4f24bd317cf17
evidence:
- path: docs/PLAN.md
  lines:
  - 10
  - 26
  blob: f73b6598e2c7ad58d8cfbb8d43696e4cb507d510
  snippet_sha1: 5a28efab07ab7a7d4db1a551e92d2683dbd2ce0e
- path: docs/PLAN.md
  lines:
  - 29
  - 52
  blob: f73b6598e2c7ad58d8cfbb8d43696e4cb507d510
  snippet_sha1: d882658be354fff1f1f5f4eef2e0da94b5795f35
- path: docs/GRILLING.md
  lines:
  - 49
  - 83
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 3713800feed3ab3ed154e98fc599b9302ac8d8b8
- path: docs/GRILLING.md
  lines:
  - 93
  - 110
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 41f0342d90ef039899b5a2126a480e43864e87bc
- path: docs/GRILLING.md
  lines:
  - 152
  - 161
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 241e8f971b7129fc8479e7d85cbe3e865ea022ee
- path: docs/GRILLING.md
  lines:
  - 111
  - 124
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 7ad63bfdba809a2a771f2a5dcca43b01b47f0fd3
- path: docs/GRILLING.md
  lines:
  - 189
  - 226
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 5c540cb1cc847930a749b8e4b15faa2a22f3c36c
- path: crates/lime-asr/src/lib.rs
  lines:
  - 3
  - 11
  blob: b9ad7c9c13c6b61d35aaa6bd9702fa7c029ec2bf
  snippet_sha1: d738d9862bfae697d71e5052608eafc0f1243eee
- path: docs/GRILLING.md
  lines:
  - 227
  - 258
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: bac18e48c56e0c3f8c9d5b9a3bfb57fb74ff98f0
- path: crates/lime-analyze/src/chime.rs
  lines:
  - 260
  - 292
  blob: 49186aea57ad7ae52ef17c7ca6178698bd44c831
  snippet_sha1: df8fc20bca73c236aaffd3f019ad1fc8d129e51d
- path: docs/GRILLING.md
  lines:
  - 270
  - 283
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: e65e36452925f90320c6fd9f091d34fa25bef5fb
- path: docs/GRILLING.md
  lines:
  - 284
  - 292
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: 268e026978ae81d701f93dd7ea9935d17a27c997
- path: docs/PLAN.md
  lines:
  - 222
  - 244
  blob: f73b6598e2c7ad58d8cfbb8d43696e4cb507d510
  snippet_sha1: 0e6bacada8803ef7b697d5f551d4f6afc43a5ebb
- path: crates/lime-store/src/lib.rs
  lines:
  - 67
  - 119
  blob: c4c1919af6abfd6e586a14aa00e9dd6682a9726a
  snippet_sha1: a020a3c711af79c269c7745d7e10da08c1a3c713
- path: docs/GRILLING.md
  lines:
  - 295
  - 303
  blob: ffe923cfda1043fb475093f23ab75408f3b40db6
  snippet_sha1: abca3a069cf3931c239b37e786ddbea79db05d07
type: decision
---
# 背景

项目在实现前先用四个真实素材做了证据化质询，明确 v1 范围与“明确不做”的项：v1 包含两级切分、题间暂停/循环、变速不变调、逐词卡拉 OK、转写导出、库与缓存、便携分发；不做逐句精听、A-B 复读、跟读对比、翻译、词典、安装包与自动更新 [[cite:docs/PLAN.md:L10-L26]]。

# 关键决策

## ASR 路线

ASR 选择 whisper.cpp 预编译可执行文件作为子进程，不绑定 Rust 库也不本地编译；版本钉死到具体构建标签，并使用 `-ml 1 -sow -mc 0` 逐词模式，关闭跨窗口上下文 [[cite:docs/PLAN.md:L29-L52]]。`-dtw` 与 VAD 被明确排除，因为 DTW 会关闭 flash attention，VAD 的时间戳语义与非 VAD 相差约 1.5 秒 [[cite:docs/PLAN.md:L29-L52]]。

## 结构切分

切分以静音/能量结构为主、ASR 只做命名与校准；仅靠纯音调检测会产生每文件数百个误报，而 Whisper 文本标签是数字静音上的幻觉 [[cite:docs/PLAN.md:L29-L52]]。真正的双信号是“叮 2578 Hz → 咚 2039 Hz、间隔约 0.59 秒”的叮咚对与静音边界，两者相邻时合并并给予最高置信度 [[cite:docs/GRILLING.md:L49-L83]]。

## 反幻觉与时间戳

Whisper 的 `(bell chimes)` 标签不可信：同一段音频单独转写时不再出现，会被并入整句 [[cite:docs/GRILLING.md:L93-L110]]。逐词模式提供 10 ms 级时间戳，而普通模式只有 1 秒粒度 [[cite:docs/GRILLING.md:L152-L161]]；但逐词时间戳在长静音处仍会漂移数秒，需要吸附 [[cite:docs/GRILLING.md:L111-L124]]。

默认的跨窗口上下文会在长静音上诱发整句重复幻觉，实测同一句可重复数十次；只有 `-mc 0` 能根治，并顺带把 944 秒转写从 43 秒压到 9 秒 [[cite:docs/GRILLING.md:L189-L226]]。该结论直接写进了转写驱动 `[[wiki:system/lime-asr]]` 的参数与模块文档 [[cite:crates/lime-asr/src/lib.rs:L3-L11]]。

## 叮咚置信度

只看叮咚间隔会把“同一材料朗读第二遍”误判为新材料，因此置信度必须加入音调纯度闸门，并以“相邻材料词级 LCS 合并”作为语义兜底 [[cite:docs/GRILLING.md:L227-L258]]。实现位于 `[[wiki:system/lime-analyze]]` 的叮咚检测器 [[cite:crates/lime-analyze/src/chime.rs:L260-L292]]。

## 吸附策略

吸附策略在真实文件上做过对比，最终保留保守的单调化与静音回拉，而不是把词强行搬到地标上；漂移吸收与卡拉 OK 稳定性优先于理论上的精准对齐 [[cite:docs/GRILLING.md:L270-L283]]。

# 工程约束

质询还发现了几条实现级约束：Slint 没有可用的自动换行布局，逐词折行只能在 Rust 侧按估算宽度手动完成；signalsmith-stretch 在 1.0x 下不透明，必须旁路并丢弃预热输出；cpal 回调按交错样本计数，位置换算必须按帧并除以声道数；whisper-cli 的 JSON 是追加扩展名而不是替换扩展名 [[cite:docs/GRILLING.md:L284-L292]]。这些约束分别对应 `[[wiki:system/lime-app]]` 的折行逻辑与 `[[wiki:system/lime-audio]]` 的旁路/预热/声道处理。

# 风险与未验证项

计划中保留了一份诚实清单：变速集成质量、rusqlite 是否带 FTS5、MP3 seek 精度、无静音结构素材的退化、cublas 包在其他 GPU 上的兼容性、逐词断句在数字/缩写处出错、缺“念白/题号播报”素材等 [[cite:docs/PLAN.md:L222-L244]]。其中 FTS5 风险已经体现在存储层：当前 `[[wiki:system/lime-store]]` 没有全文索引，只有普通 B-tree 索引 [[cite:crates/lime-store/src/lib.rs:L67-L119]]。

# 一句话结论

“嘀声”确实存在，是两声铃；纯音调检测与 Whisper 文本标记都不可靠，真正稳定的是静音结构与叮咚双信号互证 [[cite:docs/GRILLING.md:L295-L303]]。
