//! 调试用：合成用例，或直接诊断真实文件某一段时间。
//!
//! 用法：
//!   cargo run -p lime-analyze --example chime_debug
//!   cargo run -p lime-analyze --example chime_debug -- <wav> <t0_s> <t1_s>
use lime_analyze::chime::{
    band_env_db, detect_chimes, median_db, rms_env_db, ChimeParams,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = ChimeParams::default();
    if args.len() >= 3 {
        run_real(&args[0], args[1].parse().unwrap(), args[2].parse().unwrap(), &p);
    } else {
        run_synth(&p);
    }
}

fn run_real(path: &str, t0: f32, t1: f32, p: &ChimeParams) {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    let sr = spec.sample_rate;
    let ch = spec.channels as usize;
    let raw: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap_or(0) as f32 / 32768.0)
        .collect();
    let mono: Vec<f32> = if ch <= 1 {
        raw
    } else {
        raw.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()
    };
    println!("file {path}  sr={sr} ch={ch}  dur={:.1}s", mono.len() as f32 / sr as f32);

    let (rms, rms_hop) = rms_env_db(&mono, sr, 20.0, 5.0);
    let (ding, ding_ms) = band_env_db(&mono, sr, p.ding_hz, p);
    let (dong, _dong_ms) = band_env_db(&mono, sr, p.dong_hz, p);

    println!("\n检出的叮咚（全部）:");
    for c in detect_chimes(&mono, sr, p) {
        println!(
            "   叮 {:>8}ms ({:6.1}dB) -> 咚 {:>8}ms ({:6.1}dB) conf={:.2}",
            c.ding_ms, c.ding_db, c.dong_ms, c.dong_db, c.confidence
        );
    }

    let dt = ding_ms;
    let rw = ((p.rise_ms / dt).round() as usize).max(1);
    let mw = ((p.median_win_s * 1000.0 / dt).round() as usize).max(1);
    let dw = ((p.decay_win_s * 1000.0 / dt).round() as usize).max(1);
    println!("\n{t0}–{t1}s 逐帧诊断（20ms）:");
    println!("     t(s)    ding   dong    rms   d-med  rise  drop  preSilence");
    let i0 = (t0 * 1000.0 / dt) as usize;
    let i1 = ((t1 * 1000.0 / dt) as usize).min(ding.len());
    for i in i0..i1 {
        if ding[i] <= -70.0 && dong[i] <= -70.0 {
            continue;
        }
        let tms = i as f32 * dt;
        let t = tms / 1000.0;
        let lo = i.saturating_sub(mw);
        let hi = (i + mw).min(ding.len());
        let med = median_db(&ding[lo..hi]);
        let rise = i >= rw && ding[i] - ding[i - rw] >= p.rise_db;
        let lim = (i + dw).min(ding.len());
        let seg = &ding[i..lim];
        let peak = seg.iter().copied().fold(f32::MIN, f32::max);
        let pk_i = seg.iter().position(|v| *v == peak).unwrap_or(0);
        let drop = peak - seg[pk_i..].last().copied().unwrap_or(peak);
        let a = ((tms - p.pre_silence_s * 1000.0) / rms_hop).max(0.0) as usize;
        let b = (((tms - 150.0) / rms_hop).max(0.0) as usize).min(rms.len());
        let pre = if b > a {
            rms[a..b].iter().copied().fold(f32::MIN, f32::max)
        } else {
            99.0
        };
        let ri = ((tms / rms_hop) as usize).min(rms.len() - 1);
        println!(
            "  {t:8.3}  {:6.1} {:6.1} {:6.1}  {:6.1}  {rise:>5}  {drop:6.1}  {pre:8.1}",
            ding[i], dong[i], rms[ri], ding[i] - med
        );
    }
}

fn run_synth(p: &ChimeParams) {
    let sr = 48_000u32;
    let mut x = vec![0.0f32; sr as usize * 5];
    let mut tone = |buf: &mut Vec<f32>, at_s: f32, f0: f32, amp: f32, len_s: f32| {
        let a = (at_s * sr as f32) as usize;
        let n = (len_s * sr as f32) as usize;
        for i in 0..n {
            if a + i >= buf.len() {
                break;
            }
            let t = i as f32 / sr as f32;
            buf[a + i] += amp * (-t / 0.25).exp() * (2.0 * std::f32::consts::PI * f0 * t).sin();
        }
    };
    tone(&mut x, 1.0, 2578.0, 0.25, 1.0);
    tone(&mut x, 1.59, 2039.0, 0.18, 1.0);
    println!("params: {p:?}");
    println!("合成叮咚结果: {:#?}", detect_chimes(&x, sr, p));
}
