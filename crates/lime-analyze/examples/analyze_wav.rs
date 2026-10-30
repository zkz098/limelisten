//! 调试用：直接对一个 wav 跑「静音结构 + 叮咚 + 两级章节」，打印章节树。
//! 与 `lime-app --analyze` 同构，但不依赖 lime-audio/cpal，便于在无音频设备的机器上验收切分。
//!
//! 用法：cargo run -p lime-analyze --example analyze_wav -- <wav>
use lime_analyze::{
    build_chapters, detect_chimes, detect_structure, digital_silence_spans, energy_envelope,
    AnalyzeParams,
};

fn main() {
    let path = std::env::args().nth(1).expect("用法: analyze_wav <wav>");
    let params = AnalyzeParams::default();
    let mut reader = hound::WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let raw: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap_or(0) as f32 / 32768.0)
        .collect();
    let mono: Vec<f32> = if ch <= 1 {
        raw
    } else {
        raw.chunks(ch)
            .map(|c| c.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    let env = energy_envelope(&mono, spec.sample_rate, &params);
    let mut structure = detect_structure(&env, spec.sample_rate, &params);
    structure.has_digital_silence = !digital_silence_spans(&mono, spec.sample_rate, 500).is_empty();
    let chimes = detect_chimes(&mono, spec.sample_rate, &params.chime);
    let chapters = build_chapters(&structure, &[], &chimes, &params);

    let mins = chapters.iter().filter(|c| c.is_material()).count();
    println!(
        "file {path}  sr={} ch={ch} dur={:.1}s  叮咚 {}  材料 {mins}",
        spec.sample_rate,
        mono.len() as f32 / spec.sample_rate as f32,
        chimes.len()
    );
    for c in &chapters {
        println!(
            "  {} {:<10} {:>8.1} – {:>8.1}  conf={:.2}  {}",
            if c.is_material() { "MAS" } else { " Q " },
            c.title,
            c.start_ms as f64 / 1000.0,
            c.end_ms as f64 / 1000.0,
            c.confidence,
            if c.locked { "[lock]" } else { "" }
        );
    }
}
