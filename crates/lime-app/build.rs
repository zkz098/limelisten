fn main() {
    slint_build::compile("../../ui/app.slint").expect("slint build failed");
    embed_windows_resources();
}

/// 把 assets/limelisten.{rc,ico} 编成资源、链进 limelisten.exe：
/// exe 属性里的版本信息 + 资源管理器/快捷方式/任务栏显示的真图标（不用启动进程就能看到）。
///
/// 用 embed-resource 而不是 winres/winresource：它会自己按 $RC → Win10 SDK 注册表 →
/// vswhom → PATH 的顺序找 rc.exe，不会被「本机 VS18 探测不到」（见根目录 build.ps1 注释）
/// 那个坑波及；非 Windows 构建时它会自己跳过。
fn embed_windows_resources() {
    #[cfg(windows)]
    {
        use std::env;

        // 版本号只写 Cargo.toml 一处，这里转成 rc.exe 要的两种形式注进去
        let ver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
        let mut parts: Vec<u32> = ver.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        parts.resize(4, 0);

        println!("cargo:rerun-if-changed=assets/limelisten.rc");
        println!("cargo:rerun-if-changed=assets/limelisten.ico");

        let result = embed_resource::compile(
            "assets/limelisten.rc",
            embed_resource::ParamsMacrosAndIncludeDirs(
                [
                    format!("VER_NUM={}, {}, {}, {}", parts[0], parts[1], parts[2], parts[3]),
                    format!("VER_STR=\"{ver}\""),
                ],
                ["assets"], // 让 rc.exe 能在 assets/ 下找到 limelisten.ico
            ),
        );

        // 这里刻意不用 manifest_optional()：找不到 rc.exe 的情况它也算 Ok，
        // 结果就是「图标悄悄没了」。图标是发布物的一部分，宁可构建失败。
        match result {
            embed_resource::CompilationResult::Ok | embed_resource::CompilationResult::NotWindows => {}
            other => panic!("Windows 资源（图标/版本信息）嵌入失败：{other}"),
        }
    }
}
