# TODOS — limelisten 未完成工作

> 状态基线：P0 + P1 已完成（DTW 时间戳吸附、本地 SQLite 库与秒开缓存、题级微调与人工锁定防覆盖、字幕导出 SRT/LRC/TXT、GUI 现代化升级）。
>
> 历史决策与实测证据：`docs/PLAN.md`、`docs/GRILLING.md`。
> 本文只记「接下来要做什么」，以及**踩过的坑（不要重犯）**。

---

## 0. 验收命令（每次改完先跑这几条）

```powershell
.\build.ps1 test                                                              # 15 个单测全绿
.\target\debug\limelisten.exe --assert samples\*.mp3                          # 4 个素材全部 100% PASS
.\target\debug\limelisten.exe --playtest samples\训练2.mp3                     # 引擎：seek/变速/暂停点
.\target\debug\limelisten.exe --transcribe samples\训练2.mp3 10                # ASR + DTW吸附对比（偏差 < 1s）
.\target\debug\limelisten.exe --tools                                         # whisper/模型定位
.\target\debug\limelisten.exe                                                 # GUI
```

⚠️ **必须用 `build.ps1`**（而不是直接 `cargo`）：本机 VS 18 的 `vswhere` 返回空，
rustc 探测不到 MSVC，会在 `link.exe not found` 上失败。脚本先导入 `vcvars64` 再调 cargo。

---

## P1 — 核心体验与精度对齐（已全部完成 ✅）

### P1-1 时间戳吸附：DTW / Needleman-Wunsch 单调对齐 + 分段线性映射 ✅
- **实现**：`crates/lime-analyze/src/snap.rs` 实现动态规划单调序列对齐 `align_markers_dtw` 与分段线性插值 `apply_piecewise_linear`。
  - 代价矩阵引入局部斜率门控 `[0.65, 1.6]` 过滤异常跳变；
  - 保留端点外推与 `clamp_into_speech` 语音岛裁剪保底。
- **实测验收**：在 `训练2.mp3` 上成功匹配 5 对地标（原为 0 对拒绝），`Text one` 误差由 3659 ms 直降至 **0 ms**（44620 ms 对齐叮咚 44620 ms），全局残差均 `< 1s`；4 个素材 `--assert` 全部 100% PASS。

### P1-2 库管理与秒开接线 ✅
- **实现**：
  - `lime-store` schema 升级至 V2，增加 `sentence` 与 `word` 表及联合索引；
  - GUI 打开文件夹扫描支持 `upsert_media` 批量入库，媒体库选项卡直观展示音频文件、时长与听力次数；
  - 秒开缓存：启动/点击已分析媒体优先命中 SQLite 库，直接呈现章节与字幕（耗时 < 100ms，无需重新跑 Whisper）；
  - 断点续播与统计：自动记录与恢复播放进度，循环完成自动累加 `bump_stat`。

### P1-3 题级修正 UI ✅
- **实现**：
  - `ui/app.slint` 增加章节微调交互面板，提供起止点 `±0.1s / ±0.5s` 精准步进；
  - 支持「光标处拆分」、「合并下一节」、「试听末尾 2s 边界」；
  - 手动微调结果实时落库 SQLite，标记 `source = 'manual', locked = 1`，后续重新自动切分时保留人工修正不被覆盖。

### P1-4 字幕导出 SRT / LRC / TXT ✅
- **实现**：
  - `ui.on_export_subtitles` 支持将吸附后的字幕导出为标准 SRT、LRC 或纯文本 TXT，并唤起系统文件保存对话框。

### P1-5 .limed 预切分转译缓存与 Slim 纯听版架构 ✅
- **`.limed` 缓存设计与实现**：
  - 采用 8 字节魔数头（`LIMED` + 版本 1 + zstd 压缩标识）+ zstd 级 3 压缩的标准 JSON 数据包；
  - 15 分钟长音频（41 章节 + 202 句字幕 + 词级时间戳）压缩后仅 **19.2 KB**（解压 < 1ms）；
  - 加载音频时优先自动探测并秒开同名同目录下符合的 `.limed` 文件，免切分、免 Whisper 转写；支持直接打开 `.limed` 文件联动同名音频；
  - CLI 提供 `--limed <音频>` 离线批量生成与 `--show-limed <文件>` 查看工具；GUI 顶部提供「保存 .limed」按钮。
- **Slim 纯听版架构**：
  - 通过 Cargo Feature `whisper` 彻底解耦 `lime-asr`；
  - 编译 `.\build.ps1 slim`（`--no-default-features`）生成纯播放版，移除 >1.2GB 的 Whisper 与模型包，二进制包仅 ~20MB（体积缩减 98%），专用于低配设备直接消费 `.limed` 缓存。

### P1-6 Whisper 硬件测速与模型管理模态框（含轻量模型一键下载） ✅
- **GUI 测速与模型管理模态框**：
  - 顶部工具栏增加「测速与模型」入口，提供居中半透明遮罩模态框；
  - 完整呈现 `turbo`、`small`、`base`、`tiny` 四档规格卡片；
  - 界面直观显示已安装模型状态、文件体积、测速倍速判定徽章与汇总推荐；
  - Slim 纯听版自适应展示专属免模型说明卡片。
- **轻量模型后台下载与平滑进度条**：
  - 支持用户在 GUI 内直接点击「下载模型」安装更小规格模型（`base` ~142MB、`tiny` ~75MB、`small` ~466MB、`turbo` ~547MB）；
  - 下载基于 Windows 原生内置的 `curl.exe`（支持断点与重定向，免窗口后台静默运行），采用 `hf-mirror.com` 国内高速镜像并支持 `huggingface.co` 自动回退；
  - 守护线程实时汇报瞬时下载百分比与速度（`MB/s`），提供动态进度条；下载完成后自动校验并原子切换为「已就绪」状态。
- **一键异步测速与防卡死熔断**：
  - 模态框提供「开始测速」一键评测已就绪模型，后台截取 15 秒基准测试切片；
  - 贯彻 **3:1 建议标准**（3 分钟音频需 1 分钟内完成，倍速 $\ge 3.0\times$ 标记推荐）；
  - 贯彻 **RTF > 1.0 熔断保护**（单模型推理耗时超过 15.0 秒时由看门狗立即调用 `child.kill()` 主动杀死进程），杜绝低配机器长期挂起阻塞。

---

## GUI 打磨升级 ✅
- **图标库引入**：引入本地 Remix Icon 矢量字体库（`ui/assets/remixicon.ttf`），封装 `RiIcon` 与 `RiButton`，全面替换 emoji 与原生控件；
- **色彩与风格**：全面升级为 Slate / Zinc 现代化设计，优化视觉层次与高对比度字体排版；
- **选项卡切换**：左栏支持「章节导航」与「媒体库」平滑切换；
- **卡拉OK与微交互**：修复 `ScrollView` 自动拉伸导致的 350px 巨型长条异常，当前播放句逐词以紧凑琥珀色芯片呈现，点击任意词即可精准跳转该词时刻；
- **转写延迟修复**：消除人声强行吸附叮咚导致的 1~3 秒抢跑漂移，保持真实 1.0x 物理语速与开头静音自适应对齐；
- **播放条**：底部时间轴显示当前时刻与总时长（`00:00 / 00:00`），包含播放状态、微调控制台与导出快捷入口。

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
