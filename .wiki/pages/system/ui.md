---
id: system/ui
title: Slint 界面结构
status: verified
generated_at: 2026-11-01T03:28:05Z
commit: a1601d77cd134732efe85d81a5f4f24bd317cf17
evidence:
- path: crates/lime-app/build.rs
  lines:
  - 1
  - 4
  blob: 03cd5470c432acf42cb3390fc85ae8a6eb2e300c
  snippet_sha1: 74487231e1642087610d858dc714c3ac6659a176
- path: ui/app.slint
  lines:
  - 1
  - 46
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 551c084f0792c82297ed3eecc258b6816559f091
- path: ui/app.slint
  lines:
  - 47
  - 60
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 22c8f16356b6135fcf865df33ca39fa72d95b0b5
- path: ui/app.slint
  lines:
  - 96
  - 108
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: fe757262e02ff12fde96b11fe8f0a086de4e672a
- path: ui/app.slint
  lines:
  - 109
  - 112
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: afd8f5040f1aa22fa3f09518a7e4ab1768989471
- path: ui/app.slint
  lines:
  - 549
  - 554
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 203d331f43633884664659fda8f291c8e1a7f8a4
- path: ui/app.slint
  lines:
  - 222
  - 234
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: dd7d0ef5542e6f0794a4b994c734062953cee3be
- path: ui/app.slint
  lines:
  - 642
  - 706
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 3061ca0287bd16d9002ab8832415e7ea323424f5
- path: ui/app.slint
  lines:
  - 657
  - 657
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: f63751fadfa2434067cddf94633920bfec0617a7
- path: ui/app.slint
  lines:
  - 699
  - 706
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 4b5628730a7878678ebc374757403869ca9b86b3
- path: ui/app.slint
  lines:
  - 725
  - 778
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: c538355776ef83cac0aa7ea31c1f9c9fae0cc7f5
- path: ui/app.slint
  lines:
  - 959
  - 1013
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: 9adb426322b0ef31448b82bfeb816199e9acc22b
- path: ui/app.slint
  lines:
  - 1340
  - 1500
  blob: 76f960cebc58f543eb157c44f392d2f139636e91
  snippet_sha1: a2ff19c0259822c3a609e592725ad1e96d42498a
- path: crates/lime-app/src/gui.rs
  lines:
  - 803
  - 824
  blob: 0b142eceebeae8a925819af6a611337adc36d1f0
  snippet_sha1: cf6136ade56585d87ba0fe347b25031358ff5e43
- path: crates/lime-app/src/gui.rs
  lines:
  - 2280
  - 2295
  blob: 0b142eceebeae8a925819af6a611337adc36d1f0
  snippet_sha1: 8dc1ec06d878b0c21af7c3ed114942caf8b3cd91
type: system
---
# 职责

UI 由单个 Slint 文件 `ui/app.slint` 描述，构建脚本在编译期把它编译进二进制 [[cite:crates/lime-app/build.rs:L1-L4]]。界面本身不持有业务逻辑，所有状态通过属性传入、所有交互通过回调交给 `lime-app` 的 GUI 控制器。

# 结构

## 图标与基础控件

`global Ri` 把 Remixicon 字形码点集中声明为字符串常量，供所有控件复用 [[cite:ui/app.slint:L1-L46]]。`RiButton` 是基于 `Rectangle` 的图标按钮，暴露 `icon`、`text`、`icon-color`、`primary`、`enabled` 与 `clicked()` [[cite:ui/app.slint:L47-L60]]。`RiIcon` 则只负责按字形码点渲染图标 [[cite:ui/app.slint:L96-L108]]。

## 音频可视化与字幕

`AudioVisualizer` 接收 `playing` 等属性，用于渲染播放状态动画 [[cite:ui/app.slint:L109-L112]]。`SubtitleStage` 接收嵌套模型 `[[WordCell]]` 行、当前高亮词下标与原文显示开关，并向外发出 `click-word(int)` 与 `toggle-transcript()` [[cite:ui/app.slint:L549-L554]]，逐词卡拉 OK 的渲染边界集中在这里。

## 试题视图

`ExerciseView` 接收 `ExerciseQuestionRow` 数组、材料标题与交卷状态，向外发出选择选项、交卷/重置、跳转题目、打开导入/手动录入、编辑与删除题目等回调 [[cite:ui/app.slint:L222-L234]]。这说明试题的交互完全由 Rust 侧状态驱动，Slint 只负责展示与事件上抛。

## 主窗口

`MainWindow` 是唯一导出组件，集中声明播放、章节、字幕、转写、模型下载、试题与弹窗状态 [[cite:ui/app.slint:L642-L706]]。侧栏用 `sidebar-tab` 区分“章节导航”和“媒体库” [[cite:ui/app.slint:L657-L657]]，主区域用 `main-tab` 区分“逐词字幕”“听力试题”“题文对照” [[cite:ui/app.slint:L699-L706]]。

主窗口回调覆盖文件/文件夹打开、媒体选择、分析、转写、保存 `.limed`、播放控制、章节跳转、变速、循环、自动暂停、字幕导出、逐词点击、试题导入与编辑、模型基准/下载以及关于弹窗 [[cite:ui/app.slint:L725-L778]]。

侧栏两个 Tab 通过按钮点击直接切换 `sidebar-tab` [[cite:ui/app.slint:L959-L1013]]；主区域三个 Tab 同样切换 `main-tab`，并按当前值挂载 `SubtitleStage`、`ExerciseView` 或“试题 + 字幕”并排布局 [[cite:ui/app.slint:L1340-L1500]]。

# 与 Rust 的边界

Rust 侧在启动时设置版本、GitHub 地址与许可证属性，并注册打开文件等回调 [[cite:crates/lime-app/src/gui.rs:L803-L824]]。播放位置、字幕行与进度由后台消息经通道送回 UI 线程后写入这些属性 [[cite:crates/lime-app/src/gui.rs:L2280-L2295]]。因此 Slint 文件可以单独编译，但完整行为依赖 `lime-app` 注入的属性与回调。
