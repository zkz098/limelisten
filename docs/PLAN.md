# limelisten — 实现方案（证据化，2026-xx）

一个本地听力播放器：**自动按题切分 + 题间跳转/暂停 + Whisper 文本提取 + 逐词卡拉OK字幕**。
技术栈：**Rust + Slint**（Win64 / MSVC，绿色便携）。

**证据来源**：`docs/GRILLING.md`（4 个真实素材 + 实测数据）。本文件里每条关键决策都标注了依据，未验证项单列在 §9。

---

## 1. 范围

### v1 做
| 能力 | 具体 |
|---|---|
| 切分 | **两级**：材料（Passage/Text）→ 题号；依据 = 静音结构为主 + ASR 语义（`(bell chimes)`/念白）校准 |
| 播放 | 题间自动暂停、单题循环 N 次、上一题/下一题、上一材料/下一材料、**变速不变调**（0.75–1.5x 优先质量） |
| 字幕 | 双栏（左=音频时间轴/章节，右=逐词卡拉OK高亮），点击句子/词跳转 |
| 文本 | Whisper 转写 → 句/词模型；导出 SRT / LRC / TXT |
| 检索 | 全文搜索（跨库）、生词本（**词 + 例句 + 原声切片 + 时间戳**，不做词典查询） |
| 修正 | 章节列表微调（±0.1s / ±0.5s、合并、拆分）+ 边界试听 |
| 库 | 文件夹扫描成库；SQLite 存进度 + 分析缓存（不重复跑 Whisper） |
| 分发 | 绿色便携：exe + data + models + tools 同目录；模型首次运行下载 |

### v1 明确不做
逐句精听模式、A-B 复读、跟读录音对比、中文翻译（右栏只放英文原文）、离线词典查询、安装包/自动更新。

---

## 2. 关键决策（每条都有实测依据）

| # | 决策 | 依据 |
|---|---|---|
| D1 | ASR = **whisper.cpp 预编译 exe 子进程**（不绑 Rust 绑定、不本地编译） | `whisper-cublas-12.4.0-bin-x64.zip` 在 sm_120 上实测可用：`compute capability 12.0` / `use gpu = 1` / `flash attn = 1` |
| D2 | 版本**钉死 `b5130`**，不用 `latest` | v1.9.4/v1.9.3 release 附件数为 **0**；附件只挂在 `b####` 标签 |
| D3 | 转写模式 = **`-ml 1 -sow -mc 0` 逐词**，句子自己重组 | 普通模式时间戳只有 **1s** 粒度；逐词模式 **10ms**（45 种毫秒余数）；文本质量一致。`-mc 0` 关掉跨窗口上下文：实测默认 `-mc -1` 在长静音上整句重复（`9.2` 出现 `"三个月之后…"`×43 / `"听话"`×38，`训练1` 出现 `"the last chapter of the month."`×17），`-mc 64` 仍循环，只有 0 干净且快 ~4.7×（GRILLING §2.8） |
| D4 | **不用 `-dtw`** | DTW 会关掉 flash attention（`dtw_token_timestamps is not supported with flash_attn`），时间戳退化为整秒 |
| D5 | **不用 VAD** | turbo 逐词 RTF **0.026**（60s→1.6s），不缺速度；VAD 时间戳语义与非 VAD 有 ~1.5s 差异，规避 |
| D6 | 默认模型 = **large-v3-turbo q5_0**（547MB）+ Silero VAD 备用 | 实测 RTF 0.026（逐词）/0.20（普通）；用户选择 |
| D7 | 切分主依据 = **静音/能量结构**，ASR 只做命名与校准 | 精确数字静音（≤-200 dBFS）+ 规律 ~10s 答题间隔（54–89 个/文件）；**嘀声纯音调检测误报 475–758 个/文件**，不可用 |
| D8 | **叮咚检测为主（材料边界）、静音结构为辅，两者相邻时合并（conf 0.98）** | 实测签名：叮 2578 Hz → 咚 2039 Hz，Δ≈0.59 s，总在答题间隔末尾；四个文件检出 12/13/17/15 对，与静音边界完全重合。DSP 参数已标定（见 chime.rs） |
| D9 | 题号 = **按顺序推断**（可被念白正则覆盖） | 4 个素材全量转写中 `Text/Section/Question/Directions` 命中数全为 0 |
| D9b | **whisper 时间戳要做分段线性吸附**：以 DSP 语音岛为骨架，把岛内词时间戳按比例映射回真实区间 | 实测漂移：真实 +2.46 s 有声音，逐词模式却标 0.00（早 2.46 s），后段偏 4 s+；VAD 又晚 2.5 s 且压缩。不吸附则卡拉OK 会逐渐跑偏 |
| D14 | 题级（第二层）= 材料内 **2–8 s** 的间隙（你的选择）；弱叮咚（conf<0.35）不参与切分 | 你选定口径；实测本批素材里 2–8 s 间隙每文件只有 0–9 个（主要靠后续人工拆分），故参数面板可随时下调到 1.0 s |
| D15 | 帧移/窗长必须用**真实毫秒值**做时间换算（不得用名义值） | 44.1 kHz 下 5 ms = 220.5 样本 → 取 221 ⇒ 到 151 s 已漂 340 ms，导致同一文件 48 k 检出 16 个叮咚、44.1 k 只检出 1 个 |
| D10 | 模型下载 = **hf-mirror 为主 + 官方回退**，带 SHA256 | `huggingface.co` 直连在 `Invoke-WebRequest` 下 401（curl 正常）；hf-mirror 实测 200 且完成 547MB 下载。注意仓库名是 `ggerganov/whisper.cpp` |
| D11 | 便携 = 同时带 **cublas 版 + blas 版**，自动探测回退 | cublas 包 643MB 解压；无 N 卡机器需要 CPU 兵底（用户选择） |
| D12 | 音频格式 = **纯 Rust 解码（symphonia）**，不绑 ffmpeg | 素材只有 mp3；便携包不想多 100MB |
| D13 | 不做翻译、不做词典 | 用户明确选择 |
| D16 | 字幕缓存带**转写管线指纹**（`asr-pipeline/2|ml=1|sow=1|mc=0|lang=en|model=…`）：SQLite `analysis.params_hash` 存全指纹、`.limed` 的 `meta.asr_fp` 存参数指纹；打开文件时不一致→保留章节、丢弃字幕 | 否则升级后用户仍会看到 `-mc -1` 时代的重复幻觉字幕（GRILLING §2.8） |

---

## 3. 工程结构

```
D:\limelisten\
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ lime-app/               # bin：Slint UI + 应用编排（唯一写 UI 的 crate）
│  ├─ lime-audio/             # 解码/输出/transport/变速/位置时钟
│  ├─ lime-analyze/           # 结构检测(DSP) + 句子重组 + 章节构建
│  ├─ lime-asr/               # whisper-cli 驱动、JSON 解析、模型/DLL 下载与校验
│  ├─ lime-store/             # SQLite(库/进度/缓存) + FTS 搜索 + 生词本
│  └─ lime-core/              # 领域模型与错误类型（无 IO）
├─ ui/*.slint                 # Slint 界面
├─ tools/whisper/{cuda,cpu}/  # whisper-cli.exe + DLL（不入 git）
├─ models/                    # ggml-*.bin（不入 git）
├─ data/                      # library.db + cache/（不入 git）
├─ samples/                   # 你的真实素材（不入 git）
├─ docs/{GRILLING,PLAN}.md
└─ .recon/                    # 本次侦察脚本与频谱图（不入 git）
```

便携包最终形态：
```
limelisten-portable\
  limelisten.exe  ui/  tools\whisper\cuda|blas\  models\  data\{library.db,cache}\
```

---

## 4. 分析管线（`lime-analyze` + `lime-asr`）

```
① 解码 → 单声道 + 原始采样率（分析用）          symphonia
② 能量包络（10 ms hop）→ 精确静音检测            RMS ≤ -55 dBFS 且 采样值恒 0 视为数字静音
③ 语音岛/间隙分类：                              ← D7
     < 0.35 s  : 句内停顿
     0.35–2.5 s: 句/话语边界
     2.5–5.0 s : 题边界
     ≥ 5.0 s   : 材料边界（答题间隔，实测 ~10 s）
④ 写出 16 kHz 单声道 wav → whisper-cli（`-ml 1 -sow -mc 0 -oj`）  ← D3
⑤ 解析词级 JSON → 词表（10 ms 级时间戳）
⑥ 语义扫描（可配置正则表，**默认关闭**，仅作命名提示）：← D8/D9
     `Text|Unit|Lesson|Passage\s*(\d+|[A-D])` → 显式材料编号
     `Question\s*(\d+)` / `第\s*(\d+)\s*题` → 显式题号
     `(bell chimes)` 之类**不采用**（实测是数字静音上的幻觉）
⑥b **时间戳吸附**：以③的语音岛为骨架，把每个岛内的词按比例重映射到真实区间；
     跨岛的词按岛内占比切分；吸附后计算残差并记录到分析报告 → 卡拉OK 与切点都基于吸附后时间
⑦ 章节构建：结构骨架（③）→ 材料 = `≥ material_gap` 间隙划分；
     题级 = 材料内 `≥ question_gap` 的间隙（默认不降阈值，避免把对话轮次当题，见 U11）
     confidence = f(间隙余量, 吸附残差)；低置信章节在 UI 标黄，提示人工修正
⑧ 句子重组（卡拉OK 用）：标点 + 词间 ≥0.4 s 停顿切句，单句 ≤ 12 词 / ≤ 8 s
⑨ 落库：句/词/章节 → SQLite（`data/library.db`），原始词级 JSON 落 `data/cache/<sha1(path)>/words.json`
```

**完成信号/取消**：whisper-cli 以子进程运行，`-oj` 输出文件为唯一数据源（不解析日志）；取消 = kill 进程 + 清理半成品。
**进度**：读 `-pp` 进度行（stderr）→ 进度条。

---

## 5. 数据模型（`lime-store`）

```sql
media(id PK, path UNIQUE, size, mtime, duration_ms, sample_rate, channels, added_at)
analysis(media_id PK→media, model, params_hash, words_json_path, chapters_json, created_at, version)
chapter(id PK, media_id→media, level INT /*0=材料 1=题*/, parent_id→chapter, ordinal INT,
        start_ms, end_ms, title TEXT, source TEXT /*struct|asr|manual*/, confidence REAL, locked INT)
sentence(id PK, media_id, chapter_id, ordinal, start_ms, end_ms, text)
word(id PK, sentence_id, media_id, ordinal, start_ms, end_ms, text, chapter_id)
progress(media_id PK, chapter_id, pos_ms, updated_at)
listen_stat(media_id, chapter_id, plays, loops, last_at, PK(media_id,chapter_id))
vocab(id PK, word, lemma, media_id, chapter_id, sentence_id, start_ms, end_ms, clip_path, note, created_at)
setting(k PK, v)
```

缓存键：`media_path + mtime + size + asr_model + analyze_params_hash` —— 任一变化则重跑（D 与 grep 实测：单文件分析 ~30 s，可接受）。
搜索：优先 FTS5；若 `rusqlite` bundled 不含 FTS5 则退回 `LIKE`（P2 定稿，见 §9）。

---

## 6. UI（Slint）

```
┌ 侧栏（库） ─┬─ 主区 ───────────────────────────────┐
│ 文件夹树     │  ┌ 章节列表（两级，可折叠）─────────┐ │
│ 文件列表     │  │ ▸ 材料 1  Text 1   [10:23–12:40] │ │
│  进度标记    │  │   1 ▸ [11:02–11:41] ★ conf 0.92  │ │
│ 搜索框       │  │   2 ▸ [11:41–12:20]              │ │
│ 生词本       │  └──────────────────────────────────┘ │
│             │  ┌ 字幕（左右双栏）─────────────────┐ │
│             │  │ [00:11:05] Excuse me, sir. May…  │ │ ← 逐词高亮
│             │  └──────────────────────────────────┘ │
│             │  ┌ 控制条 ──────────────────────────┐ │
│             │  │ ⏮ ⏯ ⏭  0.75x ▾  循环 [2]  ▁▂▃▄▅ │ │
└─────────────┴──────────────────────────────────────┘
```

交互要点：
- **题间自动暂停**：播到章节末 → 暂停（若开启"循环 N 次"则先循环，计数写 `listen_stat`）。
- **点击字幕词** → seek 到该词起点；**双击** → 加入生词本（切片 ±1 句，写 `vocab.clip_path`）。
- **章节微调面板**：选中章节 → `-0.5s / -0.1s / +0.1s / +0.5s / 合并下一节 / 在光标处拆分 / 试听边界 2s`；改动标 `source=manual, locked=1`，重跑分析不覆盖。
- 低置信章节黄色标记 + 一键跳到下一个低置信。

UI↔后端：Slint 只读原子/共享状态，命令走 channel；UI 线程不碰解码与 ASR。

---

## 7. 音频引擎（`lime-audio`）

候选两条路，**P0 用真实素材对比后定稿**（指标：seek 延迟、变速质量、循环无缝、CPU 占用）：

| 方案 | 说明 |
|---|---|
| A. 自建 Transport | `symphonia` 解码线程 → `signalsmith-stretch`（纯 Rust，变速不变调）→ 环形缓冲 → `cpal` 输出回调；位置用原子帧计数。控制力最强（精确 seek、无缝循环、变速），代码量中等 |
| B. rodio + 自定义 Source | 快，但变速需自己实现 `Source`，seek/循环精度受 rodio 抽象限制 |

**倾向 A**：单题循环 + 立即跳转 + 变速三者组合需要帧级控制；D13 已排除 ffmpeg，解码全走 symphonia。
变速区间：0.75–1.5x 优先质量（用户要求"真·不变调，常用区间优先"），1.5–2.0x 允许轻微听感偏差。

---

## 8. 阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0 spike** | ① workspace + Slint 窗口打开文件 ② Transport：解码/输出/seek/变速/位置上报 ③ 静音结构检测 → 两级章节骨架 ④ whisper-cli 子进程 + 逐词 JSON 解析 + `(bell chimes)` 锚点 ⑤ UI：章节列表 + 题间暂停/循环 + 卡拉OK + 点击跳转 | 4 个真实素材上切分人工抽查 **≥90%** 正确；seek 延迟 **<100 ms**；1.5x 不变调可听；turbo 逐词 15min 音频 **<60 s** |

### P0 已完成部分（分析半段）—— 断言全绿

命令：`build.ps1 build` 然后 `target\debug\limelisten.exe --assert samples\*.mp3`（exit 0 = 全绿）

| 文件 | A 边界定位 | B 章节合法性 | C 叮咚重合 | 结果 |
|---|---|---|---|---|
| 9.2听力练习一.mp3 | 12/12 (100%) | 是 | 12/12 (100%) | **PASS** |
| 听力音频1.mp3 | 12/12 (100%) | 是 | 12/12 (100%) | **PASS** |
| 训练1.mp3 | 13/13 (100%) | 是 | 12/12 (100%) | **PASS** |
| 训练2.mp3 | 16/16 (100%) | 是 | 15/15 (100%) | **PASS** |

断言含义：A = 每个材料边界都落在 ≥2 s 静音内 **或** 与一个强叮咚对齐（±1.5 s）；
B = 材料不重叠、无空章节、覆盖首尾语音，且材料之间被排除的区间必须属于“答题间隔/叮咚”；
C = 强叮咚与静音边界重合率 ≥ 80%。

### P0 已完成（分析半段 + 播放半段）—— 断言全绿

命令：`build.ps1 build`，然后 `target\debug\limelisten.exe --assert samples\*.mp3`（exit 0 = 全绿）

| 文件 | A 边界定位 | B 章节合法性 | C 叮咚重合 | 结果 |
|---|---|---|---|---|
| 9.2听力练习一.mp3 | 12/12 (100%) | 是 | 12/12 (100%) | **PASS** |
| 听力音频1.mp3 | 12/12 (100%) | 是 | 12/12 (100%) | **PASS** |
| 训练1.mp3 | 13/13 (100%) | 是 | 12/12 (100%) | **PASS** |
| 训练2.mp3 | 16/16 (100%) | 是 | 15/15 (100%) | **PASS** |

其他命令：`--playtest`（引擎自测，PASS：seek 20 ms / 1.5x 精准 / 暂停点 0 ms 超调）、
`--transcribe`（ASR 链路，训练2：1619 词 → 201 句，~17 s）、`--tools`（工具/模型定位）、`--analyze`（切分报告）。
GUI：无参数启动（已做 5 s 烟雾测试）。

已实现：`lime-core`（领域模型）、`lime-analyze`（静音结构 + **叮咚检测** + 两级章节 + **时间戳吸附** + 11 个单测）、
`lime-audio`（symphonia 解码 + **播放引擎** cpal/signalsmith-stretch + 离线渲染 + 3 个单测）、
`lime-asr`（whisper-cli 驱动 + 逐词 JSON + 句子重组）、`lime-store`（SQLite schema/存取）、
`lime-app`（Slint GUI + 四个 CLI 模式）、`ui/app.slint`、`build.ps1`。

P0 剩余：题级修正 UI（微调/合并/拆分 + 边界试听）、SRT 导出、库管理（P1/P2）。
| P1 库 | SQLite + 扫描 + 后台队列（当前插队）+ 缓存 + 进度/断点 | 50 文件批量不掉帧；重复打开秒开 |
| P2 修正与学习 | 微调/合并/拆分 + 边界试听；SRT/LRC/TXT 导出；全文搜索；生词本 + 原声切片 | 修正后重跑分析不丢人工结果 |
| P3 便携与分发 | cublas/blas 双包 + DLL 下载器（hf-mirror→官方 + SHA256）+ CPU/GPU 自动探测 + 首次运行向导 | 拷到无 N 卡机器可用（CPU 模式） |

---

## 9. 风险与未验证项（诚实清单）

| # | 项 | 影响 | 处置 |
|---|---|---|---|
| U1 | ~~`-ml 1 -sow` 下 `(bell chimes)` 是否仍作为词段出现~~ **已验** | 逐词模式下非语音段变成空白长词段（实测 0→9.14 s），文本标签直接消失 | 已按 D8 处理；空白长段仅作辅助特征 |
| U1b | ~~真实语料里到底有没有嘀声~~ **已验：有，是叮咚** | — | 已标定并实现（chime.rs），4 个文件回归验证通过 |
| U2 | `signalsmith-stretch` 与 cpal 的集成质量/延迟 | 变速体验 | P0 用 A/B 两方案实测对比 |
| U3 | `rusqlite` bundled 是否含 FTS5 | 全文搜索降级 | P1 实测；不含则编译期加 feature 或退回 `LIKE` |
| U4 | mp3 seek 精度（帧级 ~26 ms） | 跳转是否"准" | P0 实测；需要更准时按帧边界缓存索引 |
| U5 | 无静音结构素材（雅思/托福长音频） | 切分退化为乱切 | v1 依赖结构；P2 加"等长/语义锚点"退化策略 |
| U6 | cublas 12.4 包在别的 GPU/驱动上可能真失败 | 便携版不可用 | 自动探测 + 明确日志 + CPU 回退（D11） |
| U7 | 逐词重组句子在数字/缩写处切错 | 字幕断句 | 规则 + 人工修正兜底 |
| U8 | **缺"念白 TEXT XX / 题号播报"素材** | 该路径无法标定 | 需你补充样例（GRILLING §3-3）；代码保留可配置正则表但默认不启用 |
| U9 | 逐词模式大量段落 vs 单句重组的性能 | 长文件 JSON 体积 | 实测 15min ≈ 数千词，JSON < 1MB，可接受 |
| U10 | ~~whisper 时间戳漂移~~ **已解决（部分）** | 卡拉OK 跑偏、切点错位 | 已实现 Affine 兑底 + 地标对齐（带门控）+ 静音词回拉；当前实测残差 1–4 s。**待改进**：地标用全局仿射拟合不够，需 DTW 式单调对齐 |
| U12 | ~~段落结束时产生空章节~~ **已修** | 断言 B 抛出的真实缺陷 | `build_chapters` 尾部题章节加 `if end > prev` 守卫 |
| U13 | 语义之争：材料区间是否应包含答题间隔？ | 影响“上一题/下一题”是否播到 10 s 空白 | 定为**不包含**；断言 B 同步按此口径 |
| U14 | Slint 1.17 无自动换行布局 | 卡拉OK 逐词高亮排版 | 已在 Rust 侧按估算宽度手动折行（嵌套模型） |
| U15 | 1.0x 时拉伸器不透明（~100 ms 垃圾） | 开始播放/跳转时会听到噪声 | 1.0x 旁路；拉伸时丢弃预热输出 |
| U11 | 题级（第二层）在纯对话素材里语义不成立 | 可能切出"对话轮次"而非"考试题" | 已按 D14 定口径（2–8 s）；实测每文件仅得 15–25 个题级章节，需配合修正 UI |
| U12 | ~~段落结束时产生空章节~~ **已修** | 断言 B 抛出的真实缺陷 | `build_chapters` 尾部题章节加 `if end > prev` 守卫 |
| U13 | 语义之争：材料区间是否应包含答题间隔？ | 影响“上一题/下一题”是否播到 10 s 空白 | 定为**不包含**（材料 end = 答题间隔起点，下一材料 start = 咚 结束）；断言 B 同步按此口径 |
