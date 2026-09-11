# TODOS — limelisten 未完成工作

> 状态基线：P0 已完成（切分闭环 + 播放引擎 + ASR 链路 + GUI 可跑）。
> 当前唯一未达标的硬指标是**时间戳吸附精度（1–4 s 残差）**，见 P1-1。
>
> 历史决策与实测证据：`docs/PLAN.md`、`docs/GRILLING.md`。
> 本文只记「接下来要做什么」，以及**踩过的坑（不要重犯）**。

---

## 0. 验收命令（每次改完先跑这几条）

```powershell
.\build.ps1 test                                                              # 14 个单测
.\target\debug\limelisten.exe --assert samples\*.mp3                          # exit 0 = 全绿
.\target\debug\limelisten.exe --playtest samples\训练2.mp3                     # 引擎：seek/变速/暂停点
.\target\debug\limelisten.exe --transcribe samples\训练2.mp3 10                # ASR + 吸附对比
.\target\debug\limelisten.exe --tools                                         # whisper/模型定位
.\target\debug\limelisten.exe                                                 # GUI
```

⚠️ **必须用 `build.ps1`**（而不是直接 `cargo`）：本机 VS 18 的 `vswhere` 返回空，
rustc 探测不到 MSVC，会在 `link.exe not found` 上失败。脚本先导入 `vcvars64` 再调 cargo。

---

## P1 — 接下来最该做的（按价值排序）

### P1-1 时间戳吸附：改成 DTW 式单调对齐 ⭐ 最高优先

- **现状**：`crates/lime-analyze/src/snap.rs` 有 4 种策略。默认「有地标用地标，否则仿射」。
  训练2 实测 `Text one` 落 41.0 s（真值≈44.6 s）→ **残差 3.6 s**。
- **为什么不够**：Whisper 的漂移**不是线性的**（40 s 处偏 4 s，113 s 处偏 12 s），
  而全局仿射/比例都装不下。地标对齐已实现（中位残差 + 斜率一致性门控），
  但在训练2 上**主动拒绝并回退**，因为 Whisper 会把同一处念白重复识别好几次，
  且由于漂移非线性，「相邻斜率」本身就不一致。
- **要做**：把「标记 ↔ 叮咚」的配对换成**单调序列对齐**（DTW / Needleman-Wunsch 风格）：
  - 代价 = |预测时刻 − 锚点时刻|，允许跳过一个标记或一个锚点；
  - 用配对结果做**分段线性**（而不是全局一条直线）映射；
  - 保留现有 `clamp_into_speech()` 后处理。
- **验收**：在 4 个素材上，`Text N` 与对应叮咚的偏差 **< 1 s**；
  在 `--transcribe` 输出的「吸附策略对比」表里，地标列优于仿射列（现在两者相同=回退了）。
- **涉及**：`snap.rs`（`landmark_pairs` / `robust_line` / `apply_line`）、`docs/PLAN.md` U10。

### P1-2 库管理接线（`lime-store` 已写好但没接 GUI）

- **现状**：`crates/lime-store/src/lib.rs` 有完整 schema（media/analysis/chapter/sentence/word/
  progress/listen_stat/vocab/setting）、`upsert_media`、`replace_chapters`、`save_progress`、`bump_stat`，
  且 `replace_chapters` 已经**跳过 locked 行**（人工修正不会被覆盖）。**GUI 完全没用它**。
- **要做**：
  1. GUI 打开文件夹 → 扫描音频 → 写库 → 左侧列表显示（含进度标记）；
  2. 分析结果（chapters/sentences/words）落库；**命中缓存直接秒开**（现在每次都重跑 whisper）；
  3. 断点续播：启动时读 `progress`；
  4. 后台队列：导入即排队（GPU 串行），**当前播放的文件插队优先**（PLAN §8 P1）；
  5. 听数统计写 `listen_stat`（已实现 `bump_stat`）。
- **验收**：50 个文件导入后 UI 不掉帧；重开已分析文件 < 1 s 出章节和字幕；杀进程后位置不丢。
- **注意**：当前 `analysis` 表缺句/词表，需要加 `sentence` / `word` 表（PLAN §5 已设计）。

### P1-3 题级修正 UI（用户明确要求过）

- **现状**：章节列表只读；`--assert` 的三条断言保证了「不重叠、非空、边界落在静音/叮咚上」，
  但题级切分（2–8 s 间隙口径）每文件只切出 15–25 段，实际需要人工微调。
- **要做**（PLAN §6 已定交互）：
  选中章节 → `-0.5s / -0.1s / +0.1s / +0.5s / 合并下一节 / 在光标处拆分 / 试听边界 2 s`；
  改动写 `source=manual, locked=1`；重跑分析不覆盖（store 侧已支持）。
- **验收**：改完立刻生效（播放器循环区间同步更新）；重跑「切分」后人工修正仍在。
- **涉及**：`ui/app.slint`、`crates/lime-app/src/gui.rs`、`lime-store`。

### P1-4 字幕导出 SRT / LRC / TXT

- **现状**：`ui.on_export_srt(|| {})` 是空实现（GUI 里标了 TODO(P2)）。
- **要做**：用吸附后的 `Sentence` 生成 SRT（句级即可，词级时间戳可选打轴）；
  LRC 用于纯播放器；TXT 纯文本。
- **验收**：导出的 SRT 拖回播放器能对上画面（误差 < 0.5 s）。

---

## P2 — 学习功能

### P2-1 生词本（词 + 例句 + 原声切片）

- 用户选定口径：**只收藏「词 + 例句 + 原声」**，不做词典查询。
- 能力已具备：`lime_audio::render` 可截取任意区间并 `write_wav`（验收时已用于渲染测试）。
- 要做：双击字幕词 → 取 ±1 句音频切片落 `data/clips/`，写 `vocab` 表；
  生词本面板可回听；导出 CSV/Anki（可选）。
- 验收：切片时长正确、时间戳与原文对齐、重开程序仍在。

### P2-2 全文搜索

- 跨库搜句子/词，点击跳转到对应时间。
- **注意**：`rusqlite` 的 `bundled` 是否含 FTS5 需要实测（PLAN U3）；不含则退回 `LIKE`。

### P2-3 逐句精听 / A-B 复读

- P0 明确排除过（用户当时只要「题间暂停 + 单题循环」）。引擎已支持 A-B 循环
  （`Engine::set_loop(a, b, count)`），做 UI 即可。
- 注意：麦克风跟读对比（录音 + 双轨对齐）工程量另有量级，需单独评估。

---

## P3 — 便携与分发

### P3-1 模型/工具下载器

- **源策略（已实测）**：`hf-mirror.com` 为主，`huggingface.co` 回退；带 SHA256 校验。
- **必须钉版本号**：
  - whisper.cpp 二进制：`https://github.com/ggml-org/whisper.cpp/releases/download/b5130/whisper-cublas-12.4.0-bin-x64.zip`
    （**不要用 `latest`**：v1.9.4/v1.9.3 的 release **附件数为 0**，附件只挂在 `b####` 标签上）
  - 模型仓库是 `ggerganov/whisper.cpp`（**不是** `ggml-org/whisper.cpp`，后者 401）
  - 默认模型 `ggml-large-v3-turbo-q5_0.bin`（547 MB）；VAD 模型 `ggml-org/whisper-vad/.../ggml-silero-v5.1.2.bin`
- 已完成：`crates/lime-app/src/paths.rs` 的定位逻辑（`--tools` 可自检）。

### P3-2 CPU/CUDA 自动探测回退

- 便携包同时带 cublas 与 blas 两套（`tools/whisper/cuda|blas/`），
  启动时探测 GPU；无 N 卡或 CUDA 加载失败时用 blas 版并明确提示。
- 实测依据：cublas-12.4 包在 sm_120 上**可用**（`--tools` + `--transcribe` 已验证）。

### P3-3 首次运行向导

- 选模型档位（速度/精度）、选数据目录、测 GPU、跑一段样例音频验证。

### P3-4 打包脚本

- 产出 `limelisten-portable/`：`limelisten.exe` + `ui/` + `tools/whisper/{cuda,blas}/` + `models/` + `data/`。

---

## 技术债 / 已知坑（**不要重犯**）

| # | 坑 | 处置方式 |
|---|---|---|
| 1 | **gitignore 不支持行尾注释**：`/dir/  # 说明` 会让整行失效（我因此差点把 2.9 GB 的 `.tools` 提交进去） | 注释必须独立成行。改 `.gitignore` 后务必 `git check-ignore -v <路径>` 复测 |
| 2 | **signalsmith-stretch 在 1.0× 不透明**：开头 ~100 ms 输出高频垃圾（每 512 帧零交叉 9.4 → 243–312） | `Stretcher` 在 \|speed−1\|<1e-3 时**旁路**；拉伸时丢弃 `output_latency + 2 块` 预热输出 |
| 3 | **cpal 回调 `data` 是交错样本，位置换算用帧** | 回调里 `out_consumed += n / dev_ch`；否则立体声下可听位置被钉在 0（实测变 0.38 倍速） |
| 4 | **whisper-cli 输出是追加扩展名**（`x.asr` → `x.asr.json`），而 `Path::with_extension` 是替换 | 拼字符串加 `.json` |
| 5 | **帧移取整漂移**：44.1 kHz 下 5 ms = 220.5 → 221 样本（真 5.0113 ms）。用名义值换算，到 151 s 漂 340 ms | `energy_envelope`/`rms_env_db`/`band_env_db` 都返回**真实**毫秒步长，换算必须用它 |
| 6 | **Rust `f64` 残差比较**：两个候选都是「0 残差」但浮点上差 1e-10，导致 2 对退化拟合胜出 | 拟合至少 3 对；评分量化（/100 ms）后再比较 |
| 7 | **whisper `(bell chimes)` 是数字静音上的幻觉**（6/9 条落在样值恒 0 区间） | 不要用它当标记；用 DSP 叮咚 + 静音结构 |
| 8 | **念白标记位置不能取滑窗首词**（中文长句作单词时，窗口首词可能比 `Text` 早 10 s） | 取**含关键词的那个词**的起点 |
| 9 | **`-dtw` 反而更差**：会关掉 flash attention，时间戳退化成整秒 | 用 `-ml 1 -sow` 逐词模式 |
| 10 | **不用 VAD**：turbo 逐词 RTF≈0.026，不缺速度；且 VAD 与非 VAD 时间戳语义有 ~1.5 s 差异 | 保持不启用 |
| 11 | **Slint 1.17 没有自动换行布局**（编译器内有 FlexboxLayout 但未暴露为元素；`WrapLayout` 不存在） | 卡拉OK 逐词折行在 Rust 侧按估算宽度做（嵌套模型 `[[WordCell]]`） |
| 12 | **symphonia 0.6 是破坏性重构**（`AudioCodecParameters`/`AudioSpec`/`plane`） | 钉 0.5 |
| 13 | **VS 18 无法被 rustc 探测**（vswhere 返回空） | 用 `build.ps1` |
| 14 | mp3 seek 是帧级精度（~26 ms） | 当前够用；若要更准需自建帧索引 |

---

## 未验证 / 待补素材

| 项 | 影响 | 需要什么 |
|---|---|---|
| **有题号播报的素材**（"Question 1…"） | 题级语义目前只能按顺序推断（`--assert` 只能保证材料级正确） | 你提供一组素材；`lime-asr` 里规则表已写好（支持 `Text/Unit/Lesson/Passage` + 数字/英文数字） |
| **无静音结构的长音频**（雅思/托福） | 现在依赖 ≥2 s 静音切材料；连续长音频会切不开 | 需要「等长 / 语义锚点」退化策略（PLAN U5） |
| **无念白的四六级真题** | 地标对齐无输入，只能仿射兜底 | 有素材后标定；或接试卷题量元数据 |
| `rusqlite` bundled 是否含 FTS5 | 全文搜索实现方式 | 一行代码实测（P2-2） |
| 无 N 卡机器上的 blas 回退 | 便携版可用性 | 在别的机器上实测（P3-2） |

---

## 测试与 CI

- **现状**：14 个单测（chime 4 / snap 7 / analyze 3 / audio 3）+ 两个可退出码验收的 CLI。
- **要做**：
  1. 把 `--assert` / `--playtest` 接进 CI（或 git hook）；
  2. golden set：把 4 个素材的边界人工标注成 CSV，做长期回归
     （`--assert` 目前只能验「结构性正确」，不能验「切得对」——见 GRILLING §3-5 你选的「自动断言即可」）；
  3. 合成素材回归：`render_check` 已能造 440 Hz 音调验变速不变调，可扩展为「合成叮咚 + 静音」的端到端用例。

---

## 相关文档

| 文件 | 内容 |
|---|---|
| `docs/PLAN.md` | 设计、决策表（D1–D15）、风险表（U1–U15）、阶段划分 |
| `docs/GRILLING.md` | 质询全过程、实测证据、反直觉发现与**我推翻过的错误结论** |
| `crates/lime-analyze/examples/chime_debug.rs` | 叮咚检测的逐帧诊断工具（参数面板将来复用） |
| `crates/lime-audio/examples/{render_check,stretch_debug,devices}.rs` | 变速/重采样/设备能力的离线验证 |
