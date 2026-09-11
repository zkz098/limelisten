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
}
