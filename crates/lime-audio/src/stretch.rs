//! 变速不变调：signalsmith-stretch 的分块封装（交错的 f32）。
//!
//! 依据 docs/PLAN.md §7 方案 A：变速要求“真·不变调，常用区间优先”。
//! `process(input, output)` 的含义是“喂进 N 帧、取出 M 帧”，M/N 即时间伸缩比：
//! 要 1.5x 快放就是 input 1024 帧 → output 683 帧。音调由 transpose 单独控制（保持 1.0）。

use signalsmith_stretch::Stretch;

/// 固定输入块大小（帧）—— 与后续环形缓冲的粒度一致。
pub const BLOCK_FRAMES: usize = 1024;

pub struct Stretcher {
    inner: Stretch,
    channels: usize,
    sample_rate: u32,
    speed: f32,
    out_buf: Vec<f32>,
    /// 1.0x 时不进入拉伸器（见 `apply` 注释）
    bypass: bool,
}

impl Stretcher {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let inner = Stretch::preset_default(channels as u32, sample_rate);
        let mut s = Self {
            inner,
            channels,
            sample_rate,
            speed: 1.0,
            out_buf: Vec::new(),
            bypass: true,
        };
        s.apply();
        s
    }

    /// 速度倍率：1.0 = 原速，1.5 = 快放 1.5 倍（时长变短、音调不变）。
    pub fn set_speed(&mut self, speed: f32) {
        let s = speed.clamp(0.25, 4.0);
        if (s - self.speed).abs() > 1e-4 {
            self.speed = s;
            self.apply();
        }
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    fn apply(&mut self) {
        // 音调不变：transpose = 1.0；伸缩完全由 process 的输入/输出帧数比决定。
        self.inner.set_transpose_factor(1.0, None);
        // 实测：**1.0x 下这个库并不透明** —— 开头的输入窗口会输出 ~100 ms 高频垃圾
        // （每 512 帧零交叉从应有的 9.4 跳到 243–312，主频被估成 681 Hz；真值 440 Hz）。
        // 官方声明的 output_latency（2880 帧 = 60 ms）**不足以**覆盖这段瞬态。
        // 所以 1.0x 直接旁路 —— 反正原速本来就不需要拉伸。
        self.bypass = (self.speed - 1.0).abs() < 1e-3;
    }

    /// 是否处于旁路（原速直通）
    pub fn is_bypass(&self) -> bool {
        self.bypass
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 给定输入帧数，算出应取出的输出帧数。
    pub fn out_frames_for(&self, in_frames: usize) -> usize {
        ((in_frames as f32) / self.speed).round().max(1.0) as usize
    }

    /// 处理一个输入块（交错，长度必须是 channels 的整数倍），返回输出切片。
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        debug_assert_eq!(input.len() % self.channels, 0, "input must be interleaved");
        if self.bypass {
            self.out_buf.clear();
            self.out_buf.extend_from_slice(input);
            return &self.out_buf;
        }
        let in_frames = input.len() / self.channels;
        let out_frames = self.out_frames_for(in_frames);
        self.out_buf.resize(out_frames * self.channels, 0.0);
        self.inner.process(input, self.out_buf.as_mut_slice());
        &self.out_buf
    }

    /// 已喂入的输入帧对应的输出延迟（帧）。
    pub fn latency_frames(&self) -> usize {
        self.inner.output_latency()
    }

    /// 库声明的输入/输出延迟（帧）。用于决定“丢掉多少开头”以避开启动瞬态。
    pub fn input_latency(&self) -> usize {
        self.inner.input_latency()
    }

    pub fn output_latency(&self) -> usize {
        self.inner.output_latency()
    }

    /// 启动/跳转后应丢弃的**输出帧数**，否则开头一段是高频垃圾。
    /// 旁路时无需丢弃；非旁路时用 output_latency + 2 个块（实测 1.5x 丢弃 latency 即干净，
    /// 这里留余量以覆盖其它比例）。
    pub fn warmup_frames(&self) -> usize {
        if self.bypass {
            0
        } else {
            self.inner.output_latency() + 2 * BLOCK_FRAMES
        }
    }

    /// 跳转后必须重置（清掉内部缓冲），否则会残留上一处的声音。
    pub fn reset(&mut self) {
        self.inner.reset();
        self.apply();
    }

    /// 文件末尾收尾：把内部残留输出吐到 `out`。
    pub fn flush(&mut self, out: &mut [f32]) {
        self.inner.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: u32, frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    /// 时长随速度变化（1.5x ⇒ 输出帧数 ≈ 输入的 1/1.5）
    #[test]
    fn speed_changes_duration() {
        let sr = 48_000;
        for (speed, expect) in [(1.0f32, 1.0f32), (1.5, 1.0 / 1.5), (0.75, 1.0 / 0.75)] {
            let mut st = Stretcher::new(1, sr);
            st.set_speed(speed);
            let total_in = sr as usize; // 1 秒
            let mut total_out = 0usize;
            for chunk in sine(1000.0, sr, total_in, 0.5).chunks(BLOCK_FRAMES) {
                total_out += st.process(chunk).len();
            }
            let ratio = total_out as f32 / total_in as f32;
            assert!(
                (ratio - expect).abs() < 0.05,
                "speed {speed}: ratio {ratio:.3} vs expect {expect:.3}"
            );
        }
    }

    /// 音调不变：1.0x 与 1.5x 输出的主频必须一致（这是“不变调”的硬指标）
    #[test]
    fn pitch_is_preserved() {
        // 用零交叉率估计主频，避免引入 FFT 依赖
        fn dominant_freq(x: &[f32], sr: u32) -> f32 {
            let mut zc = 0usize;
            for w in x.windows(2) {
                if (w[0] <= 0.0) != (w[1] <= 0.0) {
                    zc += 1;
                }
            }
            zc as f32 * sr as f32 / (2.0 * x.len() as f32)
        }
        let sr = 48_000;
        let f0 = 440.0f32;
        let mut out = vec![];
        for speed in [1.0f32, 1.5] {
            let mut st = Stretcher::new(1, sr);
            st.set_speed(speed);
            let mut acc = Vec::new();
            for chunk in sine(f0, sr, sr as usize, 0.8).chunks(BLOCK_FRAMES) {
                acc.extend_from_slice(st.process(chunk));
            }
            // 丢掉启动瞬态（库声明的输出延迟）后再测，这才是“听得到的那部分”
            let skip = st.warmup_frames().min(acc.len() / 2);
            let n = acc.len();
            let seg = &acc[skip..n - n / 10];
            out.push(dominant_freq(seg, sr));
        }
        let (a, b) = (out[0], out[1]);
        assert!(
            (a - f0).abs() < 20.0 && (b - f0).abs() < 20.0,
            "1.0x={:.1}Hz 1.5x={:.1}Hz，期望都在 {:.0}Hz 附近（不变调）",
            a,
            b,
            f0
        );
    }
}
