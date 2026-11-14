# AGENTS.md

## 质量门控（cargo-warden）

仓库使用 [cargo-warden](https://github.com/zkz098/cargo-warden) 统一管理静态架构与代码质量门控，规则定义在 `warden.toml`，历史基线记录在 `warden-baseline.json`（只允许收敛，不允许新增超标或复杂度/行数回退）。

- 提交或交付代码前运行快速静态检查：`cargo warden check`（校验模块 DAG、CI 测试覆盖一致性 `ci-parity`、Slint 主题颜色、圈复杂度、文件长度与 `cargo fmt`）。
- 完整本地验证门控：`cargo warden gate`（在静态检查基础上串行/并行执行 `fmt`、`clippy`、`test`/`nextest` 与 `doc`）。
- 仅在有意识重构并消除了既有基线项时更新基线：`cargo warden baseline generate`（请勿为了绕过新引入的违规而刷新基线）。
- CI 中的 `warden` 任务与 `ci-parity` 会验证所有 workspace crate 均被 `.github/workflows/ci.yml` 的测试命令覆盖。
