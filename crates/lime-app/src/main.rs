#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! limelisten — 听力播放器（Rust + Slint）
//!
//! 无界面模式（自动化验收用）：
//!   limelisten --analyze  <音频>              切分报告（静音结构 + 叮咚 + 两级章节）
//!   limelisten --assert   <音频>...           P0 自动断言（exit 0 = 全绿）
//!   limelisten --transcribe <音频> [句数]     只跑 ASR 链路并打印吸附前后对比
//!   limelisten --playtest <音频>              播放引擎自测（真实开卡）
//!   limelisten --chapters <音频>              打印本地库里的两级章节树（核对章节顺序）
//!   limelisten <音频>                         直接打开该文件启动界面
//!   limelisten --tools                        打印 whisper/模型定位结果
//!
//! 默认（无参数）：启动 Slint 图形界面。

mod cli;
mod gui;
mod paths;

slint::include_modules!();

use std::path::Path;

#[cfg(windows)]
fn attach_console() {
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
        fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
        fn SetStdHandle(n_std_handle: u32, h_handle: *mut std::ffi::c_void) -> i32;
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut std::ffi::c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFFFFF4;
    const INVALID_HANDLE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
    const GENERIC_READ: u32 = 0x80000000;
    const GENERIC_WRITE: u32 = 0x40000000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;

    unsafe {
        let current_out = GetStdHandle(STD_OUTPUT_HANDLE);
        if !current_out.is_null() && current_out != INVALID_HANDLE {
            return;
        }

        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
            let handle = CreateFileW(
                conout.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if !handle.is_null() && handle != INVALID_HANDLE {
                SetStdHandle(STD_OUTPUT_HANDLE, handle);
                SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_console();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("--analyze") => {
            let f = args.get(1).expect("用法: limelisten --analyze <音频>");
            cli::cmd_analyze(f)
        }
        Some("--assert") => {
            let files: Vec<String> = args[1..].to_vec();
            assert!(!files.is_empty(), "用法: limelisten --assert <音频>...");
            cli::cmd_assert(&files)
        }
        Some("--transcribe") => {
            let f = args.get(1).expect("用法: limelisten --transcribe <音频> [句数]");
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
            cli::cmd_transcribe(f, n)
        }
        Some("--limed") => {
            let f = args.get(1).expect("用法: limelisten --limed <音频>");
            cli::cmd_export_limed(f)?;
            Ok(())
        }
        Some("--show-limed") => {
            let f = args.get(1).expect("用法: limelisten --show-limed <limed文件>");
            cli::cmd_show_limed(f)
        }
        Some("--chapters") => {
            let f = args.get(1).expect("用法: limelisten --chapters <音频>");
            cli::cmd_chapters(f)
        }
        Some("--benchmark") | Some("--bench") => {
            let sample = args.get(1).map(|s| s.as_str());
            cli::cmd_benchmark(sample)
        }
        Some("--playtest") => {
            let f = args.get(1).expect("用法: limelisten --playtest <音频>");
            playtest(Path::new(f))
        }
        Some("--tools") => {
            println!("{}", gui::tool_info());
            Ok(())
        }
        // limelisten <音频|limed>：直接带着文件启动（也方便系统“打开方式”）
        Some(p) if !p.starts_with("--") && Path::new(p).is_file() => {
            gui::run_with(Some(std::path::PathBuf::from(p)))
        }
        Some("--help") | Some("-h") => {
            println!(
                "limelisten — 听力播放器\n\n\
                 无参数：启动界面\n\
                 --analyze <音频>            切分报告\n\
                 --assert <音频>...          P0 自动断言（exit 0 = 全绿）\n\
                 --transcribe <音频> [句数]  只跑 ASR 链路\n\
                 --limed <音频>              预切分与转译并打包为同名 .limed 缓存\n\
                 --show-limed <limed文件>    查看 .limed 缓存内容\n\
                 --chapters <音频>           打印本地库两级章节树（核对章节顺序）\n\
                 --bench [音频]              Whisper 转写性能测速 (3:1 标准与防卡死熔断)\n\
                 --playtest <音频>           播放引擎自测\n\
                 --tools                     打印 whisper/模型定位\n\
                 <音频|limed>               直接启动界面并载入该文件"
            );
            Ok(())
        }
        _ => gui::run(),
    }
}

/// 引擎集成测试：真实开卡 → 放音 → 量位置推进 → 跳转 → 变速 → 自动暂停点
fn playtest(path: &Path) -> anyhow::Result<()> {
    use std::thread::sleep;
    use std::time::Duration;
    let eng = lime_audio::Engine::new(path)?;
    println!(
        "engine: src={} Hz dev={} Hz ch={} resampling={} 时长={}",
        eng.src_rate,
        eng.device_rate,
        eng.device_channels,
        eng.resampling,
        cli::fmt_ms(eng.shared().duration_ms())
    );
    let mut ok = true;

    eng.play();
    sleep(Duration::from_millis(700));
    let p0 = eng.shared().pos_ms();
    sleep(Duration::from_millis(1500));
    let p1 = eng.shared().pos_ms();
    let advanced = p1.saturating_sub(p0);
    println!("1) 播放 1.5 s：位置 {p0} → {p1} ms（推进 {advanced} ms，期望 ~1500）");
    if !(900..=2200).contains(&advanced) {
        println!("   FAIL: 推进量异常");
        ok = false;
    }

    let t0 = std::time::Instant::now();
    eng.seek_ms(60_000);
    while t0.elapsed() < Duration::from_millis(600) && eng.shared().pos_ms().abs_diff(60_000) > 900 {
        sleep(Duration::from_millis(10));
    }
    let lat = t0.elapsed();
    let p2 = eng.shared().pos_ms();
    println!("2) seek 到 60000 ms：用时 {lat:?}，位置 {p2} ms");
    if p2.abs_diff(60_000) > 1500 || lat > Duration::from_millis(400) {
        println!("   FAIL: 跳转延迟/偏差过大（要求 <400 ms 且 ±1.5 s）");
        ok = false;
    }

    eng.seek_ms(60_000);
    sleep(Duration::from_millis(200));
    eng.set_speed(1.5);
    sleep(Duration::from_millis(300));
    let q0 = eng.shared().pos_ms();
    sleep(Duration::from_millis(1500));
    let adv = eng.shared().pos_ms().saturating_sub(q0);
    println!("3) 1.5x 播放 1.5 s：位置推进 {adv} ms（期望 ~2250）");
    if !(1500..=3000).contains(&adv) {
        println!("   FAIL: 变速后推进量异常");
        ok = false;
    }
    eng.set_speed(1.0);

    let now = eng.shared().pos_ms();
    eng.set_stop_at(now + 800);
    let t1 = std::time::Instant::now();
    let mut hit = false;
    while t1.elapsed() < Duration::from_millis(2500) {
        if eng.shared().take_reached_stop() {
            hit = true;
            break;
        }
        sleep(Duration::from_millis(10));
    }
    println!(
        "4) 自动暂停点 = 当前位置+800 ms：{}",
        if hit { "按时触发" } else { "未触发" }
    );
    ok &= hit;
    let over = eng.shared().pos_ms().saturating_sub(now + 800);
    println!("   停住位置超出目标 {over} ms（希望 < 250）");
    if over > 250 {
        ok = false;
    }

    println!("5) 输出欠载次数: {}", eng.shared().underruns());
    println!("\n结论: {}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
