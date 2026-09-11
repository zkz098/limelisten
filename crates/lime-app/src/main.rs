//! limelisten — 听力播放器（Rust + Slint）
//!
//! 无界面模式（自动化验收用）：
//!   limelisten --analyze  <音频>              切分报告（静音结构 + 叮咚 + 两级章节）
//!   limelisten --assert   <音频>...           P0 自动断言（exit 0 = 全绿）
//!   limelisten --transcribe <音频> [句数]     只跑 ASR 链路并打印吸附前后对比
//!   limelisten --playtest <音频>              播放引擎自测（真实开卡）
//!   limelisten --tools                        打印 whisper/模型定位结果
//!
//! 默认（无参数）：启动 Slint 图形界面。

mod cli;
mod gui;
mod paths;

slint::include_modules!();

use std::path::Path;

fn main() -> anyhow::Result<()> {
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
        Some("--playtest") => {
            let f = args.get(1).expect("用法: limelisten --playtest <音频>");
            playtest(Path::new(f))
        }
        Some("--tools") => {
            println!("{}", gui::tool_info());
            Ok(())
        }
        Some("--help") | Some("-h") => {
            println!(
                "limelisten — 听力播放器\n\n\
                 无参数：启动界面\n\
                 --analyze <音频>            切分报告\n\
                 --assert <音频>...          P0 自动断言（exit 0 = 全绿）\n\
                 --transcribe <音频> [句数]  只跑 ASR 链路\n\
                 --playtest <音频>           播放引擎自测\n\
                 --tools                     打印 whisper/模型定位"
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
