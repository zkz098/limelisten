use std::env;
use std::fs;
use std::path::Path;
use std::process::{exit, Command};

fn print_help() {
    println!(
        r#"limelisten xtask - 统一构建与工作流管理工具

用法:
    cargo xtask [SUBCOMMAND] [OPTIONS]

常用命令:
    cargo xtask                   等价于 cargo build
    cargo xtask build [--release] 构建项目（release 模式自动注入 target-cpu 与静态 CRT）
    cargo xtask dist [--version <VER>] 构建并组装绿色便携发布包（打包至 target/dist/）
    cargo xtask run [--release]   构建并运行应用
    cargo xtask test [--release]  执行测试套件
    cargo xtask check             检查代码与类型

Release 专属优化:
    当指定 --release 或执行 dist 时，xtask 会自动注入编译参数：
      - 所有平台：-C target-cpu=x86-64-v3（启用 AVX2/FMA 等现代指令集加速）
      - Windows 平台：-C target-feature=+crt-static（静态链接 C/C++ CRT，便携运行）
"#
    );
}

fn package_dist(raw_args: &[String]) {
    let mut version = None;
    let mut iter = raw_args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--version" || arg == "-v" {
            version = iter.next().cloned();
        }
    }

    let version = version
        .unwrap_or_else(|| env::var("LIMELISTEN_VERSION").unwrap_or_else(|_| "0.1.0".to_string()));
    let version = version.trim_start_matches('v');

    let os_str = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch_str = env::consts::ARCH;

    let pkg_name = format!("limelisten-v{version}-{os_str}-{arch_str}");
    println!("== xtask dist: 开始构建便携包 [{pkg_name}] ==");

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(&cargo);
    cmd.args([
        "build",
        "--release",
        "-p",
        "lime-app",
        "--bin",
        "limelisten",
    ]);

    let is_windows = cfg!(target_os = "windows");
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
    cmd.env("RUSTFLAGS", &rustflags);
    eprintln!("== xtask dist (release 专属参数): RUSTFLAGS={}", rustflags);

    let status = cmd.status().expect("无法执行 cargo build");
    if !status.success() {
        eprintln!("== xtask dist: 构建失败");
        exit(status.code().unwrap_or(1));
    }

    let dist_root = Path::new("target").join("dist");
    let pkg_dir = dist_root.join(&pkg_name);
    let _ = fs::remove_dir_all(&pkg_dir);
    fs::create_dir_all(pkg_dir.join("tools").join("whisper")).expect("创建 tools/whisper 失败");
    fs::create_dir_all(pkg_dir.join("models")).expect("创建 models 失败");
    fs::create_dir_all(pkg_dir.join("data")).expect("创建 data 失败");

    let bin_name = if cfg!(target_os = "windows") {
        "limelisten.exe"
    } else {
        "limelisten"
    };
    let src_bin = Path::new("target").join("release").join(bin_name);
    if !src_bin.is_file() {
        eprintln!("== xtask dist: 未找到目标文件 {}", src_bin.display());
        exit(1);
    }
    fs::copy(&src_bin, pkg_dir.join(bin_name)).expect("复制可执行文件失败");

    if Path::new("LICENSE").is_file() {
        let _ = fs::copy("LICENSE", pkg_dir.join("LICENSE"));
    }
    if Path::new("README.md").is_file() {
        let _ = fs::copy("README.md", pkg_dir.join("README.md"));
    }

    let readme_content = r#"LimeListen 绿色便携版
====================
1. 运行可执行程序即可直接启动 LimeListen。
2. Whisper 转写引擎与 AI 语音模型可在软件界面「测速与模型」面板中一键下载。
3. 所有运行数据、库索引与模型将保存在本软件同级目录下，不污染系统盘。
"#;
    let _ = fs::write(pkg_dir.join("首次使用说明.txt"), readme_content);

    println!("== xtask dist: 便携包已组装就绪 -> {}", pkg_dir.display());
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

    if raw_args.first().map(|s| s.as_str()) == Some("dist") {
        package_dist(&raw_args);
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
        && !cargo_args
            .iter()
            .any(|a| a == "--bin" || a == "-p" || a == "--package")
    {
        cargo_args.insert(1, "--bin".to_string());
        cargo_args.insert(2, "limelisten".to_string());
    }

    let is_release = cargo_args.iter().any(|a| a == "--release");

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(&cargo);
    cmd.args(&cargo_args);

    if is_release {
        let is_windows =
            cfg!(target_os = "windows") || cargo_args.iter().any(|a| a.contains("windows"));

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
