use std::env;
use std::process::{exit, Command};

fn print_help() {
    println!(
        r#"limelisten xtask - 统一构建与工作流管理工具

用法:
    cargo xtask [SUBCOMMAND] [OPTIONS]

常用命令:
    cargo xtask                   等价于 cargo build
    cargo xtask build [--release] 构建项目（release 模式自动注入 target-cpu 与静态 CRT）
    cargo xtask run [--release]   构建并运行应用
    cargo xtask test [--release]  执行测试套件
    cargo xtask check             检查代码与类型

Release 专属优化:
    当指定 --release 时，xtask 会自动注入编译参数：
      - 所有平台：-C target-cpu=x86-64-v3（启用 AVX2/FMA 等现代指令集加速）
      - Windows 平台：-C target-feature=+crt-static（静态链接 C/C++ CRT，便携运行）
"#
    );
}

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();

    if raw_args.first().map(|s| s.as_str()) == Some("--help")
        || raw_args.first().map(|s| s.as_str()) == Some("-h")
        || raw_args.first().map(|s| s.as_str()) == Some("help")
    {
        print_help();
        return;
    }

    let mut cargo_args = if raw_args.is_empty() {
        vec!["build".to_string()]
    } else if raw_args[0].starts_with('-') {
        let mut v = vec!["build".to_string()];
        v.extend(raw_args);
        v
    } else {
        raw_args
    };

    // 当运行 `cargo xtask run ...` 且未显式指定目标二进制或包时，默认执行主应用 limelisten
    if cargo_args.first().map(|s| s.as_str()) == Some("run")
        && !cargo_args.iter().any(|a| a == "--bin" || a == "-p" || a == "--package")
    {
        cargo_args.insert(1, "--bin".to_string());
        cargo_args.insert(2, "limelisten".to_string());
    }

    let is_release = cargo_args.iter().any(|a| a == "--release");

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(&cargo);
    cmd.args(&cargo_args);

    if is_release {
        let is_windows = cfg!(target_os = "windows")
            || cargo_args.iter().any(|a| a.contains("windows"));

        let mut flags = Vec::new();
        if is_windows {
            flags.push("-C target-feature=+crt-static");
        }
        flags.push("-C target-cpu=x86-64-v3");

        let mut rustflags = env::var("RUSTFLAGS").unwrap_or_default();
        for flag in flags {
            if !rustflags.contains(flag) {
                if !rustflags.is_empty() {
                    rustflags.push(' ');
                }
                rustflags.push_str(flag);
            }
        }

        eprintln!("== xtask (release 专属参数): RUSTFLAGS={}", rustflags);
        cmd.env("RUSTFLAGS", rustflags);
    }

    match cmd.status() {
        Ok(status) => {
            if let Some(code) = status.code() {
                exit(code);
            } else {
                exit(1);
            }
        }
        Err(err) => {
            eprintln!("== xtask 错误: 无法启动 cargo ({}): {}", cargo, err);
            exit(1);
        }
    }
}
