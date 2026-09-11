//! 诊断：变速器在 1.0x 下的输出是否有块边界不连续（会凭空制造零交叉 → 测出假频率）。
use lime_audio::stretch::{Stretcher, BLOCK_FRAMES};

fn main() {
    let sr = 48_000u32;
    let f0 = 440.0f32;
    for channels in [1usize, 2] {
        let frames = sr as usize; // 1 秒
        let mut x: Vec<f32> = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let v = 0.8 * (2.0 * std::f32::consts::PI * f0 * i as f32 / sr as f32).sin();
            for _ in 0..channels {
                x.push(v);
            }
        }
        for speed in [1.0f32, 1.5] {
            let mut st = Stretcher::new(channels, sr);
            st.set_speed(speed);
            println!(
                "ch={channels} speed={speed}: 库声明的 input_latency={} output_latency={}",
                st.input_latency(),
                st.output_latency()
            );
            let mut y: Vec<f32> = Vec::new();
            for chunk in x.chunks(BLOCK_FRAMES * channels) {
                y.extend_from_slice(st.process(chunk));
            }
            let yf = y.len() / channels;
            // 每 512 帧的零交叉数（单声道看第 0 声道）
            let mono: Vec<f32> = (0..yf).map(|i| y[i * channels]).collect();
            let mut counts = Vec::new();
            for w in mono.chunks(512) {
                let mut zc = 0;
                for k in 0..w.len().saturating_sub(1) {
                    if (w[k] <= 0.0) != (w[k + 1] <= 0.0) {
                        zc += 1;
                    }
                }
                counts.push(zc);
            }
            let peak = mono.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            println!(
                "   out_frames={yf} peak={peak:.3} 每512帧零交叉={:?}",
                &counts[..counts.len().min(14)]
            );

            // 丢弃库声明的 output_latency 之后，是否变干净？
            let skip = st.output_latency().max(1) / channels.max(1);
            let rest = &mono[skip.min(mono.len())..];
            let mut zc = 0;
            for k in 0..rest.len().saturating_sub(1) {
                if (rest[k] <= 0.0) != (rest[k + 1] <= 0.0) {
                    zc += 1;
                }
            }
            let est = zc as f32 * sr as f32 / (2.0 * rest.len().max(1) as f32);
            println!(
                "   丢弃 {skip} 帧后：估计主频 = {est:.1} Hz（期望 440）；前 3 个窗口零交叉={:?}",
                &counts[(skip / 512).min(counts.len())..(skip / 512 + 3).min(counts.len())]
            );
        }
    }
    let expected = 2.0 * 440.0 * 512.0 / 48000.0;
    println!("\n每 512 帧的理论零交叉数 = {expected:.2}（明显偏离即是伪影）");
}
