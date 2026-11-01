---
id: quickstart
title: Wiki 快速入口
status: verified
generated_at: 2026-11-01T03:28:11Z
commit: a1601d77cd134732efe85d81a5f4f24bd317cf17
evidence: []
tags:
- generated
type: index
---
# limelisten Wiki —— 快速入口

生成的导航页,不作为源码证据;完整目录见 [[wiki:index]]。

## 推荐阅读路径

### architecture

1. [[wiki:architecture/overview]] —— LimeListen 架构总览

### workflow

1. [[wiki:workflow/build-and-release]] —— 构建与发布流程

### system

1. [[wiki:system/lime-analyze]] —— lime-analyze 结构分析
1. [[wiki:system/lime-app]] —— lime-app 应用外壳
1. [[wiki:system/lime-asr]] —— lime-asr 转写驱动
1. [[wiki:system/lime-audio]] —— lime-audio 音频引擎
1. [[wiki:system/lime-core]] —— lime-core 领域模型
1. [[wiki:system/lime-store]] —— lime-store 库与缓存
1. [[wiki:system/ui]] —— Slint 界面结构

### 其他

- [[wiki:architecture/design-decisions]] —— 设计决策与实测依据

## 如何使用本 Wiki(agent 协议)

1. 信任任何页面前先调用 `wiki_check_freshness`(或 `loredex check`);过期页面按未验证对待,直接读源码。
2. 用 `wiki_get_evidence` 核查论断 —— 每条引用都能解析到当前 git HEAD 上的精确文件内容。
3. 保持 wiki 新鲜:`wiki_plan` 列出待写/待修页面;写作时每条论断配一个行内 `[[cite:path:Lx-Ly]]` 标记;漂移的引用用 `wiki_refresh_page` 自愈。
