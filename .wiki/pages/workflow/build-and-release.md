---
id: workflow/build-and-release
title: 构建与发布流程
status: verified
generated_at: 2026-11-01T03:27:15Z
commit: a1601d77cd134732efe85d81a5f4f24bd317cf17
evidence:
- path: .cargo/config.toml
  lines:
  - 1
  - 2
  blob: 35049cbcb13c204648d1f7897162492f05123199
  snippet_sha1: 60b307f3cc97a0bca53ba836ca1c70451815b132
- path: xtask/src/main.rs
  lines:
  - 145
  - 169
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: 440768a84ba65a24ee1b619b2dc51f3ce7a7ca8f
- path: xtask/src/main.rs
  lines:
  - 165
  - 207
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: 689cbb059d48da88b46ebee22760fd51f78fd836
- path: xtask/src/main.rs
  lines:
  - 171
  - 193
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: 8b3784b3f55e69597d18196f1d102c9cc9399aa0
- path: xtask/src/main.rs
  lines:
  - 29
  - 52
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: a019871683455878da3cf0f751d943cc012d051f
- path: xtask/src/main.rs
  lines:
  - 54
  - 127
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: ffe59514ce8f4fdba91d780c7cf3ccce1249fbd5
- path: xtask/src/main.rs
  lines:
  - 120
  - 127
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: cfe7994ab7dda24e6f75fe2f1ea0439b538e9c28
- path: xtask/src/main.rs
  lines:
  - 51
  - 52
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: 3d50dbf6f58ff7864102c5a385db313693384842
- path: crates/lime-app/src/paths.rs
  lines:
  - 22
  - 45
  blob: 1e8ab95c18c27d2a7a949980e4297fc83c950b67
  snippet_sha1: c7a41eac5dc322136dc08de7b88df2c02b6c69b8
- path: .github/workflows/ci.yml
  lines:
  - 1
  - 18
  blob: 8565b348762b4ef79dfabef63712c6cd2ec738d0
  snippet_sha1: f31eebc156dfc6546d5e5252ba9927f8f78908cf
- path: .github/workflows/ci.yml
  lines:
  - 20
  - 50
  blob: 8565b348762b4ef79dfabef63712c6cd2ec738d0
  snippet_sha1: d099ae44e3346edcc42795dd8dc66eed43372c28
- path: .github/workflows/ci.yml
  lines:
  - 52
  - 88
  blob: 8565b348762b4ef79dfabef63712c6cd2ec738d0
  snippet_sha1: c1b15840d404f9495b964c651417a4faa60a563b
- path: .github/workflows/ci.yml
  lines:
  - 89
  - 96
  blob: 8565b348762b4ef79dfabef63712c6cd2ec738d0
  snippet_sha1: cf7a6e9496a1f6de4ce67581a4e222f98142b824
- path: .github/workflows/release.yml
  lines:
  - 42
  - 44
  blob: 443fb772c3cb276d50ecee1ceae25c62df27b5b9
  snippet_sha1: f461eec7ede6c39b3a5974594d1645e6129f8a31
- path: .github/workflows/release.yml
  lines:
  - 47
  - 70
  blob: 443fb772c3cb276d50ecee1ceae25c62df27b5b9
  snippet_sha1: 40fbd2c2d3e70928458f8ad2135322a323c56aa9
- path: xtask/src/main.rs
  lines:
  - 195
  - 207
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: 9c21792fd8dcb5393401f9de83acc2e71c05e91e
- path: xtask/src/main.rs
  lines:
  - 191
  - 192
  blob: 7c4ec3040ea4ac87f4b67911e43d2c10c28771f6
  snippet_sha1: fa20013da85b1ed94aa0c652975dac124db216b1
type: workflow
---
# 触发点

日常构建与发布统一走 `xtask`：`.cargo/config.toml` 把 `cargo xtask` 别名为 `cargo run -p xtask --` [[cite:.cargo/config.toml:L1-L2]]。无参数时等价于 `cargo build`；`run` 未指定目标时自动补 `--bin limelisten` [[cite:xtask/src/main.rs:L145-L169]]。

# 步骤序列

## 普通构建

除 `dist` 外的子命令都原样转发给 cargo；只替换可执行文件为 `$CARGO` 并透传退出码 [[cite:xtask/src/main.rs:L165-L207]]。当参数包含 `--release` 时，xtask 自动注入 `-C target-cpu=x86-64-v3`，Windows 下再加 `-C target-feature=+crt-static`，并去重后写入 `RUSTFLAGS` [[cite:xtask/src/main.rs:L171-L193]]。

## 便携包

`cargo xtask dist` 进入独立的 `package_dist` 流程：版本默认取 `LIMELISTEN_VERSION` 或 `0.1.3`，去掉前缀 `v`，按目标平台与架构生成 `limelisten-v<版本>-<os>-<arch>` 名称 [[cite:xtask/src/main.rs:L29-L52]]。随后以 release 模式构建 `lime-app` 并组装发布目录 [[cite:xtask/src/main.rs:L54-L127]]；包内附带首次使用说明，声明运行数据、库索引与模型都保存在程序同级目录 [[cite:xtask/src/main.rs:L120-L127]]。

# 状态与产物

`dist` 的产物位于 `target/dist/`，目录名包含版本、操作系统与架构 [[cite:xtask/src/main.rs:L51-L52]]。便携布局由 `product_root()` 决定：可执行文件旁存在 `tools/` 或 `models/` 时即视为便携根，数据写入其下 `data/` [[cite:crates/lime-app/src/paths.rs:L22-L45]]。

# CI 与发布

常规 CI 在 `main` 的 push 与 PR 上触发，但忽略 `docs/**`、`**.md`、`.gitignore` 与 `samples/**`；分为样式、测试与依赖审计三个 job [[cite:.github/workflows/ci.yml:L1-L18]]。样式 job 安装音频/GUI 依赖后执行 `cargo fmt --check` 与 `cargo clippy --locked --workspace --all-targets -- -D warnings` [[cite:.github/workflows/ci.yml:L20-L50]]。测试 job 在 Windows 与 Ubuntu 矩阵上运行 `cargo check` 与 `cargo test --workspace --all-targets`，Linux 使用 xvfb [[cite:.github/workflows/ci.yml:L52-L88]]。依赖审计 job 安装并运行 `cargo audit` [[cite:.github/workflows/ci.yml:L89-L96]]。

发布工作流由版本触发，构建 Windows 发布包时通过 `cargo xtask dist --version "$VERSION"` 组装便携目录 [[cite:.github/workflows/release.yml:L42-L44]]，随后压缩、生成校验和并发布 GitHub Release [[cite:.github/workflows/release.yml:L47-L70]]。

# 错误处理与可观测性

xtask 启动 cargo 失败时打印错误并以 1 退出；cargo 正常退出则透传其退出码 [[cite:xtask/src/main.rs:L195-L207]]。release 注入参数时会先打印最终 `RUSTFLAGS`，便于在 CI 日志中核对是否启用了 x86-64-v3 与静态 CRT [[cite:xtask/src/main.rs:L191-L192]]。
