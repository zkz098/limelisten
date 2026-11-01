---
id: architecture/overview
title: LimeListen 架构总览
status: verified
generated_at: 2026-11-01T03:27:08Z
commit: a1601d77cd134732efe85d81a5f4f24bd317cf17
evidence:
- path: crates/lime-app/src/main.rs
  lines:
  - 1
  - 15
  blob: e07d9a8170ee49129499987d964f877ee19541d7
  snippet_sha1: ebfbc6b476d3016b9a5a625421f0851c60feb1ce
- path: Cargo.toml
  lines:
  - 1
  - 8
  blob: e25cc8b508e65c43d10278ac4240381609b5d169
  snippet_sha1: 9c2dc54298a30af904e663602540d0232b1187a9
- path: crates/lime-core/src/lib.rs
  lines:
  - 1
  - 26
  blob: 82d2fd25773ca1d1f49475d5d61de3347d225f2b
  snippet_sha1: c9b8ff59433d12faa26f6b1f32384c5c2905f61c
- path: crates/lime-analyze/src/lib.rs
  lines:
  - 1
  - 7
  blob: f99cf5944540912477cb6e73ed79ba06ec7da386
  snippet_sha1: 8acc78a5c5f6c19f6404078874eea3eca86d7adc
- path: crates/lime-asr/src/lib.rs
  lines:
  - 1
  - 12
  blob: b9ad7c9c13c6b61d35aaa6bd9702fa7c029ec2bf
  snippet_sha1: b818c298880527bcb7e245b28e5173fef52bf91e
- path: crates/lime-audio/src/lib.rs
  lines:
  - 1
  - 3
  blob: 544e8cda6065e844903c5b8eb1eec0bba437673a
  snippet_sha1: 7d763c8064b25822c8bffb84af4bc428eec3f1f2
- path: crates/lime-store/src/lib.rs
  lines:
  - 1
  - 6
  blob: c4c1919af6abfd6e586a14aa00e9dd6682a9726a
  snippet_sha1: b8e1353f18aa86d7300356784bd6385af71f3f6f
- path: crates/lime-app/src/gui.rs
  lines:
  - 778
  - 815
  blob: 0b142eceebeae8a925819af6a611337adc36d1f0
  snippet_sha1: 53524079305d598b76643d30e8e923ade0f94b61
- path: crates/lime-core/src/lib.rs
  lines:
  - 5
  - 26
  blob: 82d2fd25773ca1d1f49475d5d61de3347d225f2b
  snippet_sha1: 5614bb400708ca0cdc29be1345c152590649efc0
- path: crates/lime-app/src/gui.rs
  lines:
  - 2522
  - 2555
  blob: 0b142eceebeae8a925819af6a611337adc36d1f0
  snippet_sha1: 3666645020ab6aa2e762a0f6c77b78de47420d94
- path: crates/lime-app/src/cli.rs
  lines:
  - 422
  - 458
  blob: 310ce2f0fbadfbbefda1587822a444dee76ee2d8
  snippet_sha1: fdffb01fa6e534816907b5f399535a46b03dddb3
- path: crates/lime-app/src/cli.rs
  lines:
  - 625
  - 639
  blob: 310ce2f0fbadfbbefda1587822a444dee76ee2d8
  snippet_sha1: 511f86749e712e1cb2ebfc8222c71e108771c509
- path: crates/lime-store/src/lib.rs
  lines:
  - 697
  - 716
  blob: c4c1919af6abfd6e586a14aa00e9dd6682a9726a
  snippet_sha1: a1e8872a97c79132a4f221537c09bdc88a9ebb70
- path: crates/lime-core/src/limed.rs
  lines:
  - 35
  - 40
  blob: badd6e22dbf9b4ed9d1745756505e821d23e88bc
  snippet_sha1: 905a7de8f43d561b6829d8522dae17b8431b1c31
- path: crates/lime-audio/src/engine.rs
  lines:
  - 89
  - 108
  blob: 773327c64fd6b7243c53f661bcd842a05140f7b3
  snippet_sha1: cd108c1ed8bdf7d8485dc1c5427ecea4f5734d23
- path: crates/lime-app/src/gui.rs
  lines:
  - 2280
  - 2295
  blob: 0b142eceebeae8a925819af6a611337adc36d1f0
  snippet_sha1: 8dc1ec06d878b0c21af7c3ed114942caf8b3cd91
- path: crates/lime-audio/src/decode.rs
  lines:
  - 25
  - 38
  blob: b5d1bbeb705af4b37a857c795949c0a272c7a5ca
  snippet_sha1: ef3769293ae858bef8ed6abaeea8003823baa491
- path: crates/lime-asr/src/lib.rs
  lines:
  - 133
  - 164
  blob: b9ad7c9c13c6b61d35aaa6bd9702fa7c029ec2bf
  snippet_sha1: ca0c19238f1fb31293ff7699d73d329e3909dcbe
- path: crates/lime-audio/src/engine.rs
  lines:
  - 309
  - 319
  blob: 773327c64fd6b7243c53f661bcd842a05140f7b3
  snippet_sha1: 7762628549909728f85668db08b1219a280c20a2
- path: crates/lime-store/src/lib.rs
  lines:
  - 410
  - 475
  blob: c4c1919af6abfd6e586a14aa00e9dd6682a9726a
  snippet_sha1: 3584d10f3bd1405854785eb1b508309fef42f738
- path: crates/lime-core/src/lib.rs
  lines:
  - 89
  - 137
  blob: 82d2fd25773ca1d1f49475d5d61de3347d225f2b
  snippet_sha1: 85a380ad4833eb8905a9f83165e6471489ec2eff
- path: crates/lime-app/src/paths.rs
  lines:
  - 22
  - 45
  blob: 1e8ab95c18c27d2a7a949980e4297fc83c950b67
  snippet_sha1: c7a41eac5dc322136dc08de7b88df2c02b6c69b8
- path: crates/lime-asr/src/lib.rs
  lines:
  - 172
  - 200
  blob: b9ad7c9c13c6b61d35aaa6bd9702fa7c029ec2bf
  snippet_sha1: 5f77706254476f8f7fc9d659015c916afe080053
- path: crates/lime-audio/src/decode.rs
  lines:
  - 48
  - 53
  blob: b5d1bbeb705af4b37a857c795949c0a272c7a5ca
  snippet_sha1: 1d653a1936b6a32a3c1fe9ea2e41427cf90fdcf2
type: architecture
---
# 系统目标

LimeListen 是本地听力训练播放器：自动把音频切成“材料 → 题号”两级章节，结合 Whisper 逐词转写提供卡拉 OK 字幕，并在本机保存媒体库、播放进度与分析缓存。所有入口收敛在单一 `limelisten` 二进制：无参数启动 Slint 图形界面，带参数进入分析、断言、转写、章节检查与播放自测等无界面模式 [[cite:crates/lime-app/src/main.rs:L1-L15]]。

## 模块地图与分层

工作区由 `crates/*` 与 `xtask` 组成，统一版本、Edition 与 MSRV 在根 `Cargo.toml` 声明 [[cite:Cargo.toml:L1-L8]]。

- `lime-core`：领域模型、错误类型与序列化格式，声明不依赖 IO [[cite:crates/lime-core/src/lib.rs:L1-L26]]。
- `lime-analyze`：DSP 结构分析，从能量包络推导静音、语音岛、叮咚标记与两级章节骨架 [[cite:crates/lime-analyze/src/lib.rs:L1-L7]]。
- `lime-asr`：以子进程驱动 `whisper-cli`，解析 JSON 词级时间戳并重组句子 [[cite:crates/lime-asr/src/lib.rs:L1-L12]]。
- `lime-audio`：symphonia 解码 → signalsmith-stretch 变速 → 环形缓冲 → cpal 输出 [[cite:crates/lime-audio/src/lib.rs:L1-L3]]。
- `lime-store`：SQLite 媒体库、进度、分析缓存、字幕与练习数据 [[cite:crates/lime-store/src/lib.rs:L1-L6]]。
- `lime-app`：唯一写 UI 的 crate，包含 Slint GUI、CLI 与便携路径解析 [[cite:crates/lime-app/src/gui.rs:L778-L815]]。

依赖方向是单向的：`lime-app` 编排所有能力，`lime-analyze`、`lime-asr`、`lime-audio`、`lime-store` 依赖 `lime-core` 的模型与错误类型，而 `lime-core` 不向上依赖 [[cite:crates/lime-core/src/lib.rs:L5-L26]]。

## 数据流

打开一个音频时，GUI 先尝试匹配的 `.limed` 缓存，其次读取 SQLite 中的章节与字幕，只有两者都缺失才启动后台分析 [[cite:crates/lime-app/src/gui.rs:L2522-L2555]]。分析链路在 CLI 与 GUI 间复用：`cmd_analyze` 流式解码并打印静音、叮咚与章节报告 [[cite:crates/lime-app/src/cli.rs:L422-L458]]，`cmd_transcribe` 则先导出分析用 WAV，再调用 ASR 并打印吸附前后的词时间戳 [[cite:crates/lime-app/src/cli.rs:L625-L639]]。

分析结果以“管线指纹”作为缓存键持久化：SQLite 的 `analysis` 表同时保存模型名与 `params_hash` [[cite:crates/lime-store/src/lib.rs:L697-L716]]，`.limed` 文件则在元数据中保存 `asr_fp`，空串表示旧缓存或外部工具生成 [[cite:crates/lime-core/src/limed.rs:L35-L40]]。

播放侧由音频线程内的 `Shared` 原子状态与命令通道驱动；UI 只发送 `Play/Pause/Seek/SetSpeed/Loop/StopAt/Quit` 命令并轮询原子量 [[cite:crates/lime-audio/src/engine.rs:L89-L108]]。GUI 用重复触发的 Slint Timer 从 crossbeam 通道取消息并刷新位置、字幕与进度 [[cite:crates/lime-app/src/gui.rs:L2280-L2295]]。

## 故障域

- 解码失败统一映射为 `Error::Decode`，cpal 与 symphonia 的底层错误被字符串化后向上返回 [[cite:crates/lime-audio/src/decode.rs:L25-L38]]。
- ASR 失败区分“无法启动子进程”“Windows 缺少 VC++ 运行库”“非零退出码”与“读取输出失败”，全部映射为 `Error::Asr` [[cite:crates/lime-asr/src/lib.rs:L133-L164]]。
- 播放欠载只在实际播放时计数，暂停期间的 xrun 不会污染诊断数据 [[cite:crates/lime-audio/src/engine.rs:L309-L319]]。
- 章节存储顺序异常时会回退到 rowid 排序并重新规范化，避免坏数据直接破坏 UI [[cite:crates/lime-store/src/lib.rs:L410-L475]]。

## 关键不变量

- 章节向量必须满足 `seq == 下标`、材料在前、题目只能挂在当前材料下；`normalize_chapters` 负责幂等修复 [[cite:crates/lime-core/src/lib.rs:L89-L137]]。
- 便携模式优先使用可执行文件同级的 `tools/` 或 `models/`，否则回退到开发目录；该规则决定工具、模型与数据的最终位置 [[cite:crates/lime-app/src/paths.rs:L22-L45]]。
- 字幕缓存携带转写管线指纹，参数变化时必须丢弃字幕而保留章节 [[cite:crates/lime-asr/src/lib.rs:L172-L200]]。
- 解码显式关闭 gapless，以保证解码时间轴与缓存的章节/字幕对齐 [[cite:crates/lime-audio/src/decode.rs:L48-L53]]。
