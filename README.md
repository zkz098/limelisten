# LimeListen 🍋

面向听力训练的本地智能音频播放器。

基于 **Rust + Slint** 构建，集成了精确音频结构分析（静音/能量分析、叮咚材料边界检测）、**Whisper** 逐词级转写、时间戳吸附与逐词高亮卡拉OK字幕。

---

## ✨ 核心特性

- **结构智能切分**：双层章节体系（材料篇章 → 题号），基于声学特征（数字静音、纯音调叮咚检测）与语义对齐自动切分。
- **专为听力优化**：支持题间自动暂停、单题无缝循环、0.75x–1.5x 高保真变速不变调播放。
- **逐词卡拉OK字幕**：双栏界面，左侧音频时间轴与材料章节导航，右侧逐词高亮卡拉OK，点击任意词/句子毫秒级跳转。
- **Whisper 离线转写**：支持调用外部 Whisper 引擎，软件内提供 CPU / BLAS / CUDA 多版本一键下载与模型测速。
- **绿色便携设计**：单文件免安装，运行数据、模型与缓存均保存在程序同级目录。

---

## 🛠️ 构建与运行

### 依赖环境
- [Rust](https://rustup.rs/) (1.92+ 推荐)
- Linux 构建依赖：`libasound2-dev`, `libfontconfig1-dev`, `libxkbcommon-dev`

> 老显卡 / 低功耗一体机若遇到启动黑屏或 OpenGL 驱动报错，可设置环境变量
> `SLINT_BACKEND=winit-software` 强制使用软件渲染（包内已内置该回退）。

### 开发与构建
本项目使用内置的 `xtask` 作为统一工作流工具：

```bash
# 检查与编译
cargo xtask check
cargo xtask build

# 运行主应用
cargo xtask run

# 运行全套单元测试
cargo xtask test

# 构建 Windows / Linux 便携发布包（打包至 target/dist/）
cargo xtask dist
```

---

## 📜 开源协议

本项目采用 [GPL-3.0-or-later](LICENSE) 协议开源。
