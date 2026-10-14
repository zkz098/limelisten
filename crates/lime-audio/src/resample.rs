//! 采样率转换（仅在设备不接受文件采样率时的兜底路径）。
//!
//! 实测本机 WASAPI 输出接受 44100 / 48000（crates/lime-audio/examples/devices.rs），
//! 所以正常路径**不做重采样**；这里提供一个线性插值实现，保证极端情况下仍能出声。

pub struct LinearResampler {
    channels: usize,
    /// 每个输出帧前进多少输入帧（= src_rate / dst_rate）
    step: f64,
    pos: f64,
}

impl LinearResampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Self {
        Self {
            channels,
            step: src_rate as f64 / dst_rate as f64,
            pos: 0.0,
        }
    }

    pub fn needed_input_frames(&self, out_frames: usize) -> usize {
        (self.pos + self.step * out_frames as f64).ceil() as usize + 1
    }

    /// 在只有 `in_frames` 帧可用输入时，最多能安全产出多少输出帧。
    ///
    /// 流式重采样时用它决定每次取多少输出，保证 `process` 不会越界读取
    /// （`needed_input_frames` 是访问下标的严格上界，故反之也成立）。
    pub fn max_output_frames(&self, in_frames: usize) -> usize {
        if in_frames < 2 {
            return 0;
        }
        let mut out = (((in_frames as f64) - 1.0 - self.pos) / self.step).floor() as usize;
        // 浮点安全边际：逐步回退直到确实放得下
        while out >= 2 && self.needed_input_frames(out) > in_frames {
            out -= 1;
        }
        out
    }

    /// `input`/`output` 均为交错的 f32；返回实际消耗的输入帧数。
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        let ch = self.channels;
        let in_frames = input.len() / ch;
        let out_frames = output.len() / ch;
        if in_frames < 2 || out_frames == 0 {
            return 0;
        }
        let mut consumed = 0usize;
        for o in 0..out_frames {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            if i + 1 >= in_frames {
                for c in 0..ch {
                    output[o * ch + c] = input[(in_frames - 1) * ch + c];
                }
                continue;
            }
            for c in 0..ch {
                let a = input[i * ch + c];
                let b = input[(i + 1) * ch + c];
                output[o * ch + c] = a + (b - a) * frac;
            }
            self.pos += self.step;
            consumed = i + 1;
        }
        self.pos -= consumed as f64;
        consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 44.1k → 48k 后正弦的零交叉率（= 频率）必须保持
    #[test]
    fn frequency_is_preserved() {
        let (src, dst, f0) = (44_100u32, 48_000u32, 1000.0f32);
        let x: Vec<f32> = (0..src as usize)
            .map(|i| (2.0 * std::f32::consts::PI * f0 * i as f32 / src as f32).sin())
            .collect();
        let out_frames = dst as usize / 2;
        let mut y = vec![0.0f32; out_frames];
        let mut r = LinearResampler::new(src, dst, 1);
        let mut done = 0usize;
        let mut consumed = 0usize;
        while done < out_frames {
            let want = (out_frames - done).min(4096);
            let need = r.needed_input_frames(want).min(x.len() - consumed);
            if need < 2 {
                break;
            }
            let c = r.process(&x[consumed..consumed + need], &mut y[done..done + want]);
            consumed += c;
            done += want;
        }
        let mut zc = 0;
        for w in y[..done].windows(2) {
            if (w[0] <= 0.0) != (w[1] <= 0.0) {
                zc += 1;
            }
        }
        let est = zc as f32 * dst as f32 / (2.0 * done as f32);
        assert!((est - f0).abs() < 25.0, "resampled freq {est:.1} Hz, expect ~{f0}");
    }

    /// `max_output_frames` 必须保证 `process` 不越界读取（否则流式分块会 panic）。
    #[test]
    fn max_output_frames_never_overreads() {
        for src in [22_050u32, 32_000, 44_100, 48_000] {
            let r = LinearResampler::new(src, 16_000, 1);
            for n in 2..2048 {
                let out = r.max_output_frames(n);
                assert!(
                    out < 2 || r.needed_input_frames(out) <= n,
                    "src={src} in={n} out={out} needs={}",
                    r.needed_input_frames(out)
                );
            }
        }
    }

    /// 流式分块（模拟解码逐包喂入 + 循环 drain）的结果应与整段一次处理一致：
    /// 频率保持，且不会因跨块边界出现野值/恐慌。
    #[test]
    fn streaming_chunks_preserve_frequency() {
        let (src, dst, f0) = (48_000u32, 16_000u32, 1000.0f32);
        let x: Vec<f32> = (0..src as usize * 2)
            .map(|i| (2.0 * std::f32::consts::PI * f0 * i as f32 / src as f32).sin())
            .collect();
        let mut r = LinearResampler::new(src, dst, 1);
        let mut pending: Vec<f32> = Vec::new();
        let mut y: Vec<f32> = Vec::new();
        let mut buf: Vec<f32> = Vec::new();
        for chunk in x.chunks(997) {
            pending.extend_from_slice(chunk);
            loop {
                let want = r.max_output_frames(pending.len()).min(4096);
                if want < 2 {
                    break;
                }
                buf.resize(want, 0.0);
                let c = r.process(&pending, &mut buf);
                assert!(c > 0, "process 必须前进，否则会死循环");
                y.extend_from_slice(&buf);
                pending.drain(..c);
            }
        }
        // 输出帧数应接近 src 的 1/3
        let expect = x.len() / 3;
        assert!(
            (y.len() as i64 - expect as i64).abs() < 8,
            "输出 {} 帧，期望 ~{expect}",
            y.len()
        );
        let mut zc = 0usize;
        for w in y.windows(2) {
            if (w[0] <= 0.0) != (w[1] <= 0.0) {
                zc += 1;
            }
        }
        let est = zc as f32 * dst as f32 / (2.0 * y.len() as f32);
        assert!((est - f0).abs() < 25.0, "streaming freq {est:.1} Hz, expect ~{f0}");
    }
}
