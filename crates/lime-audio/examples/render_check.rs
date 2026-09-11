//! 无 UI 验收：验证“变速不变调”与 seek 正确性。
//!
//! 用法：cargo run -p lime-audio --example render_check -- <某个音频文件>
//! 会合成一个 440 Hz 的音调文件做硬指标校验，再在给定文件上量测变速时长比。

use lime_audio::render::{dominant_freq_zero_cross, render, write_wav, RenderOptions};
use std::path::Path;

fn main() {
    let tmp = std::env::temp_dir().join("lime_render_check");
    std::fs::create_dir_all(&tmp).unwrap();

    // ---- 1. 合成 440 Hz / 3 秒测试音 ----
    let rate = 48_000u32;
    let f0 = 440.0f32;
    let src: Vec<f32> = (0..rate as usize * 3)
        .map(|i| 0.8 * (2.0 * std::f32::consts::PI * f0 * i as f32 / rate as f32).sin())
        .collect();
    let tone_path = tmp.join("tone440.wav");
    // 单声道文件：顺带验证“单声道 → 双声道”的归一化
    write_wav(&tone_path, &src, 1, rate).unwrap();

    println!("=== 合成 440 Hz 单声道 3.000 s ===");
    let mut ok = true;
    for speed in [1.0f32, 1.5, 0.75] {
        let opt = RenderOptions {
            speed,
            target_rate: rate,
            channels: 2,
            max_ms: None,
            start_ms: 0,
        };
        let y = render(&tone_path, &opt).unwrap();
        let ch = 2usize;
        // 取左声道，丢掉首尾各 10%（延迟/收敛区）
        let frames = y.len() / ch;
        let left: Vec<f32> = (0..frames).map(|i| y[i * ch]).collect();
        let seg = &left[frames / 10..frames - frames / 10];
        let dur = y.len() as f32 / ch as f32 / rate as f32;
        let est = dominant_freq_zero_cross(seg, rate);
        let expect_dur = 3.0 / speed as f32;
        let dur_ok = (dur - expect_dur).abs() < 0.25;
        let pitch_ok = (est - f0).abs() < 15.0;
        ok &= dur_ok && pitch_ok;
        println!(
            "  speed={speed:4}  时长={dur:5.2}s（期望≈{expect_dur:4.2}s {}）  主频={est:6.1}Hz（期望≈{f0:.0}Hz {}）",
            if dur_ok { "OK" } else { "偏差大" },
            if pitch_ok { "OK" } else { "跑调" }
        );
    }

    // ---- 2. 在真实文件上量变速时长比 ----
    if let Some(p) = std::env::args().nth(1) {
        println!("\n=== 真实文件：{} ===", p);
        let path = Path::new(&p);
        for speed in [1.0f32, 1.5] {
            let opt = RenderOptions {
                speed,
                target_rate: 48_000,
                channels: 2,
                max_ms: Some(12_000),
                start_ms: 0,
            };
            let t0 = std::time::Instant::now();
            let y = render(path, &opt).unwrap();
            let secs = y.len() as f32 / 2.0 / 48_000.0;
            println!(
                "  speed={speed:4}  渲染 {secs:5.2}s 音频，用时 {:?}（12 s 源素材）",
                t0.elapsed()
            );
        }
        // ---- 3. seek 正确性：从 60 s 处渲染 2 s，检查非静音 ----
        let opt = RenderOptions {
            speed: 1.0,
            target_rate: 48_000,
            channels: 2,
            max_ms: Some(2_000),
            start_ms: 60_000,
        };
        let y = render(path, &opt).unwrap();
        let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        println!("  seek 到 60 s 渲染 2 s：峰值 {peak:.3}（应 > 0）");
        ok &= peak > 0.001;
    }

    println!("\n结论: {}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
