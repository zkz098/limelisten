//! 叮咚标记检测（已在 4 个真实素材上标定验证）。
//!
//! 实测签名（`docs/GRILLING.md` §2.1b）：
//! - **叮 = 2578 Hz**，**咚 = 2039 Hz**，两声间隔 **Δ ≈ 0.59–0.60 s**
//! - 每一对都出现在**答题间隔（≥2 s 近数字静音）的末尾**，即材料分界处
//! - 电平：立体声素材 −16.1 / −22.8 dB；单声道素材 −21.2 / −24.8 dB
//!
//! 判定用的判别式（缺一不可，实测可把误报从 900+ 压到 12–14 个）：
//! 1. 快速起振（120 ms 内上升 ≥10 dB）
//! 2. 显著高于局部中位（±5 s 内 ≥10 dB）
//! 3. **单调衰减**（峰值后 1.2 s 内下降 ≥14 dB，且中途不回升 >4 dB）—— 语音不会这样
//! 4. **前面是静音**（起振前 2 s 的宽带 RMS ≤ −70 dB）—— 这一条把语音谐波全部排除
//! 5. **音调纯度**（带内峰值 ≈ 同时刻宽带 RMS，见 `ChimeParams::tonality_db`）——
//!    真叮咚是近纯音，带内能量几乎等于总能量；语音/爆破的起振是宽带的，带内比宽带低
//!    10–22 dB。这一条专门拦「同一材料朗读第二遍处」的弱瞬态：它们的叮→咚间隔可能
//!    恰好落在 0.59 s 附近，仅靠前面四条会被误判为材料边界，把一份材料切成两份。
//!
//! 实现用二阶带通（biquad）+ 滑动包络，O(n)，1 GB 级音频也只需几百毫秒。

use crate::EnvelopeAccumulator;

/// 二阶带通滤波器（RBJ cookbook）。
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn bandpass(fs: f32, f0: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        // transposed direct form II
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// 检测参数（放进 AnalyzeParams，可在参数面板里调）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChimeParams {
    pub ding_hz: f32,
    pub dong_hz: f32,
    pub q: f32,
    pub env_ms: f32,
    pub rise_ms: f32,
    pub rise_db: f32,
    pub over_median_db: f32,
    pub median_win_s: f32,
    pub decay_win_s: f32,
    pub decay_drop_db: f32,
    pub no_rerise_db: f32,
    /// 起振前考察窗口长度（秒）
    pub pre_silence_s: f32,
    /// 窗口尾部排除量：必须排掉标记自己的起振（实测坑：44.1k 下起振点晚 60ms，
    /// 导致 [-150ms] 的旧窗口把标记自身算进"前导静音"，从而漏检）
    pub pre_exclude_ms: f32,
    /// 前导段必须比标记自身电平低这么多 dB（相对判定，不依赖绝对音量）
    pub pre_relative_db: f32,
    /// 前导段的绝对上限（实测本类素材前导是真数字静音）
    pub pre_silence_db: f32,
    pub pair_min_s: f32,
    pub pair_max_s: f32,
    /// 理想间隔（实测 0.59 s），用于置信度打分
    pub pair_ideal_s: f32,
    /// 间隔偏差达到此值时置信度归零
    pub pair_conf_tol_s: f32,
    /// 低于此置信度的叮咚不用于切分（仍在报告/参数面板里列出，供人工判断）。
    /// 实测：conf=0 的几处是语音停顿里的弱谐波（Δ 偏离 0.59 s 很多），
    /// 把它们当真会把材料数从 12 涨到 18。
    pub min_confidence: f32,
    /// **音调纯度闸门**：叮的**带内峰值**相对该峰值处的宽带 RMS 至少要高出这么多 dB
    /// （负值即允许带内略低于宽带）。真叮咚 ≈0 dB（近纯音），语音/噪声起振低 10–22 dB。
    /// 实测四个素材：真的全部 ≈0，假的 ≤−10，取 −8 有充足余量。
    pub tonality_db: f32,
}

impl Default for ChimeParams {
    fn default() -> Self {
        Self {
            ding_hz: 2578.0,
            dong_hz: 2039.0,
            q: 18.0,
            env_ms: 20.0,
            rise_ms: 120.0,
            rise_db: 10.0,
            over_median_db: 10.0,
            median_win_s: 5.0,
            decay_win_s: 1.2,
            decay_drop_db: 14.0,
            no_rerise_db: 4.0,
            pre_silence_s: 2.0,
            pre_exclude_ms: 300.0,
            pre_relative_db: 12.0,
            // 实测本类素材的前导是真数字静音（≤-200 dB）；留出余量取 -65 dB，
            // 既能滤掉“短停顿里的伪标记”（实测 12 → 28 个的误报源），又能容受轻度底噪。
            pre_silence_db: -65.0,
            pair_min_s: 0.25,
            pair_max_s: 1.2,
            pair_ideal_s: 0.59,
            pair_conf_tol_s: 0.30,
            min_confidence: 0.35,
            tonality_db: -8.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chime {
    pub ding_ms: u64,
    pub dong_ms: u64,
    pub ding_db: f32,
    pub dong_db: f32,
    /// 0.0–1.0：间隔接近 0.59 s 且两声都清晰 → 高置信
    pub confidence: f32,
}

impl Chime {
    /// 材料正文通常从第二声之后开始（实测：叮咚结束后 ∼0.1–0.5 s 起播）。
    pub fn content_start_ms(&self) -> u64 {
        self.dong_ms
    }
}

/// 宽带 RMS 包络（dB）。返回 **真实** 帧移（ms）——
/// 注意：帧移按采样点数取整，所以真实帧移可能不等于名义值
/// （例：44.1 kHz 下 5 ms = 220.5 样本 → 取 221 → 真实 5.0113 ms）。
/// 时间→下标换算**必须**用返回值，否则会累积漂移（实测在 151 s 处漂移 340 ms，
/// 让“前导静音”窗口滑到标记自己身上，从而漏检）。
pub fn rms_env_db(x: &[f32], sr: u32, frame_ms: f32, hop_ms: f32) -> (Vec<f32>, f32) {
    let mut acc = EnvelopeAccumulator::new(sr, frame_ms, hop_ms);
    acc.push(x);
    acc.finish()
}

/// 单频带包络（dB）。返回 **真实** 时间步长（ms，= 窗长取整后的实际毫秒数），
/// 时间→下标换算必须用它（理由同 `rms_env_db`）。
pub fn band_env_db(x: &[f32], sr: u32, f0: f32, params: &ChimeParams) -> (Vec<f32>, f32) {
    let mut acc = BandEnvelopeAccumulator::new(sr, f0, params);
    acc.push(x);
    acc.finish()
}

/// 流式单频带包络：biquad 状态与 20 ms 窗口累积量一直保留，
/// 不再为整段音频额外分配一份滤波结果（长音频上可省下数百 MB）。
pub struct BandEnvelopeAccumulator {
    bp: Biquad,
    win: usize,
    sample_rate: u32,
    acc: f64,
    fill: usize,
    env: Vec<f32>,
}

impl BandEnvelopeAccumulator {
    pub fn new(sr: u32, f0: f32, params: &ChimeParams) -> Self {
        let sr = sr.max(1);
        let win = ((params.env_ms / 1000.0) * sr as f32).round().max(1.0) as usize;
        Self {
            bp: Biquad::bandpass(sr as f32, f0, params.q),
            win,
            sample_rate: sr,
            acc: 0.0,
            fill: 0,
            env: Vec::new(),
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &v in samples {
            let y = self.bp.process(v);
            self.acc += (y as f64) * (y as f64);
            self.fill += 1;
            if self.fill == self.win {
                let ms = self.acc / self.win as f64;
                self.env.push((10.0 * (ms + 1e-20).log10()) as f32);
                self.acc = 0.0;
                self.fill = 0;
            }
        }
    }

    pub fn dt_ms(&self) -> f32 {
        self.win as f32 / self.sample_rate as f32 * 1000.0
    }

    pub fn finish(self) -> (Vec<f32>, f32) {
        let dt = self.dt_ms();
        (self.env, dt)
    }
}

pub fn median_db(v: &[f32]) -> f32 {
    median_of(v)
}

fn median_of(v: &[f32]) -> f32 {
    if v.is_empty() {
        return -200.0;
    }
    let mut s: Vec<f32> = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[s.len() / 2]
}

/// 在一个频带上找"起振 + 高于局部中位 + 单调衰减 [+ 前面静音]"的事件。
/// 返回 (时间 ms, 峰值 dB)，供检测器与参数面板调试用。
pub fn detect_band(
    env: &[f32],
    env_ms: f32,
    rms: &[f32],
    rms_hop_ms: f32,
    params: &ChimeParams,
    require_pre_silence: bool,
) -> Vec<(f32, f32)> {
    if env.is_empty() {
        return Vec::new();
    }
    let dt_ms = env_ms;
    let rw = ((params.rise_ms / dt_ms).round() as usize).max(1);
    let mw = ((params.median_win_s * 1000.0 / dt_ms).round() as usize).max(1);
    let dw = ((params.decay_win_s * 1000.0 / dt_ms).round() as usize).max(1);
    let mut out = Vec::new();
    let mut i = rw;
    while i < env.len() {
        if env[i] - env[i - rw] >= params.rise_db {
            let lo = i.saturating_sub(mw);
            let hi = (i + mw).min(env.len());
            if env[i] - median_of(&env[lo..hi]) >= params.over_median_db {
                let lim = (i + dw).min(env.len());
                let seg = &env[i..lim];
                let peak = seg.iter().copied().fold(f32::MIN, f32::max);
                let pk_i = seg.iter().position(|v| *v == peak).unwrap_or(0);
                let post = &seg[pk_i..];
                let no_re = post.iter().all(|v| *v <= peak + params.no_rerise_db);
                let drop = peak - post.last().copied().unwrap_or(peak);
                let t_ms = i as f32 * dt_ms;
                // 带内峰值的绝对时刻：衰减窗最长 1.2 s，里面可能跟过后出现的另一个
                // 宽带瞬态（如朗读第二遍的起振）；音调纯度必须拿“峰值处”的宽带对比，
                // 不能拿起振处的宽带。
                let peak_t_ms = (i + pk_i) as f32 * dt_ms;
                let pc = (((peak_t_ms - 40.0).max(0.0) / rms_hop_ms) as usize).min(rms.len());
                let pd = (((peak_t_ms + 200.0) / rms_hop_ms) as usize).min(rms.len());
                let peak_rms = if pd > pc {
                    rms[pc..pd].iter().copied().fold(f32::MIN, f32::max)
                } else {
                    -200.0
                };
                // 音调纯度：真叮咚是近纯音，带内峰值 ≈ 同时刻宽带；语音/噪声起振是宽带的，
                // 带内比宽带低 10–22 dB（见模块头 §5）。
                let tonal_ok = peak >= peak_rms + params.tonality_db;
                let pre_ok = if require_pre_silence {
                    let a =
                        (((t_ms - params.pre_silence_s * 1000.0) / rms_hop_ms).max(0.0)) as usize;
                    let b = (((t_ms - params.pre_exclude_ms) / rms_hop_ms).max(0.0) as usize)
                        .min(rms.len());
                    let pre_max = if b > a {
                        rms[a..b].iter().copied().fold(f32::MIN, f32::max)
                    } else {
                        99.0
                    };
                    pre_max <= params.pre_silence_db && pre_max <= peak_rms - params.pre_relative_db
                } else {
                    true
                };
                if no_re && drop >= params.decay_drop_db && pre_ok && tonal_ok {
                    out.push((t_ms, peak));
                    i = lim;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// 找叮咚对：叮（第一声，前面必须是静音）→ 咚（第二声，间隔 0.25–1.2 s）。
fn peak_with_decay(
    env: &[f32],
    dt_ms: f32,
    start_ms: f32,
    end_ms: f32,
    params: &ChimeParams,
) -> Option<(f32, f32)> {
    let a = ((start_ms / dt_ms).ceil() as usize).min(env.len());
    let b = ((end_ms / dt_ms).floor() as usize).min(env.len());
    if b <= a + 2 {
        return None;
    }
    let mw = ((params.median_win_s * 1000.0 / dt_ms).round() as usize).max(1);
    let dw = ((params.decay_win_s * 1000.0 / dt_ms).round() as usize).max(1);
    // 取窗口内最响的点作为候选：叭的瞬态泄露落在本窗口之外（见上），
    // 而真正的咚一定是这段窗口里最响的。
    let (im, peak) = env[a..b]
        .iter()
        .enumerate()
        .fold(
            (a, f32::MIN),
            |(bi, bp), (k, v)| {
                if *v > bp {
                    (a + k, *v)
                } else {
                    (bi, bp)
                }
            },
        );
    if peak <= f32::MIN {
        return None;
    }
    let lo = im.saturating_sub(mw);
    let hi = (im + mw).min(env.len());
    if env[im] - median_of(&env[lo..hi]) < params.over_median_db {
        return None;
    }
    let lim = (im + dw).min(env.len());
    let post = &env[im..lim];
    let no_re = post.iter().all(|v| *v <= peak + params.no_rerise_db);
    let drop = peak - post.last().copied().unwrap_or(peak);
    if !no_re || drop < params.decay_drop_db {
        return None;
    }
    Some((im as f32 * dt_ms, peak))
}

pub fn detect_chimes(x: &[f32], sr: u32, params: &ChimeParams) -> Vec<Chime> {
    let (rms, rms_hop) = rms_env_db(x, sr, 20.0, 5.0);
    let (ding_env, ding_ms) = band_env_db(x, sr, params.ding_hz, params);
    let (dong_env, dong_ms) = band_env_db(x, sr, params.dong_hz, params);
    detect_chimes_from_envelopes(
        &rms, rms_hop, &ding_env, ding_ms, &dong_env, dong_ms, params,
    )
}

/// 由已经算好的包络做叮咚配对。流式分析路径用它复用单遍解码得到的包络。
#[allow(clippy::too_many_arguments)]
pub fn detect_chimes_from_envelopes(
    rms: &[f32],
    rms_hop: f32,
    ding_env: &[f32],
    ding_ms: f32,
    dong_env: &[f32],
    dong_ms: f32,
    params: &ChimeParams,
) -> Vec<Chime> {
    let dings = detect_band(ding_env, ding_ms, rms, rms_hop, params, true);

    let mut out = Vec::new();
    for (d_t, d_db) in dings {
        let from = d_t + params.pair_min_s * 1000.0;
        let to = d_t + params.pair_max_s * 1000.0;
        let Some((g_t, g_db)) = peak_with_decay(dong_env, dong_ms, from, to, params) else {
            continue;
        };
        let delta = (g_t - d_t) / 1000.0;
        let err = (delta - params.pair_ideal_s).abs();
        let conf = (1.0 - (err / params.pair_conf_tol_s)).clamp(0.0, 1.0);
        out.push(Chime {
            ding_ms: d_t as u64,
            dong_ms: g_t as u64,
            ding_db: d_db,
            dong_db: g_db,
            confidence: conf,
        });
    }
    out.sort_by_key(|c| c.ding_ms);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成一个叮咚：2578 Hz 0.15 s + 失谐衰减，0.59 s 后 2039 Hz 0.15 s，前后加静音。
    #[test]
    fn detects_synthetic_chime() {
        let sr = 48_000u32;
        let mut x = vec![0.0f32; sr as usize * 5]; // 5 s
        let tone = |buf: &mut [f32], at_s: f32, f0: f32, amp: f32, len_s: f32| {
            let a = (at_s * sr as f32) as usize;
            let n = (len_s * sr as f32) as usize;
            for i in 0..n.min(buf.len() - a) {
                let t = i as f32 / sr as f32;
                // 指数衰减，模拟铃声拖尾
                buf[a + i] += amp * (-t / 0.25).exp() * (2.0 * std::f32::consts::PI * f0 * t).sin();
            }
        };
        tone(&mut x, 1.0, 2578.0, 0.25, 1.0);
        tone(&mut x, 1.59, 2039.0, 0.18, 1.0);
        let chimes = detect_chimes(&x, sr, &ChimeParams::default());
        assert_eq!(
            chimes.len(),
            1,
            "should find exactly one chime, got {chimes:?}"
        );
        let c = chimes[0];
        assert!(
            (c.ding_ms as i64 - 1000).abs() < 60,
            "ding at {} ms",
            c.ding_ms
        );
        assert!(
            (c.dong_ms as i64 - 1590).abs() < 80,
            "dong at {} ms",
            c.dong_ms
        );
        assert!(c.confidence > 0.7, "confidence {}", c.confidence);
    }

    /// 纯语音式噪声（快速起振但持续不衰减）不应被当成叮。
    #[test]
    fn rejects_sustained_noise() {
        let sr = 32_000u32;
        let mut x = vec![0.0f32; sr as usize * 4];
        let a = sr as usize; // 1 s 起
        for i in 0..sr as usize * 2 {
            let t = i as f32 / sr as f32;
            let n = ((i as f32 * 12.9898).sin() * 43_758.547).fract() - 0.5; // 伪随机
            x[a + i] = 0.3 * n + 0.2 * (2.0 * std::f32::consts::PI * 2578.0 * t).sin();
        }
        let chimes = detect_chimes(&x, sr, &ChimeParams::default());
        assert!(
            chimes.is_empty(),
            "sustained noise must not match, got {chimes:?}"
        );
    }

    /// 流式带通/RMS 包络必须与整段计算一致（否则切分边界会漂）。
    #[test]
    fn streaming_envelopes_match_batch() {
        let sr = 48_000u32;
        let params = ChimeParams::default();
        let mut x = vec![0.0f32; sr as usize * 2];
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            if (0.5..1.2).contains(&t) {
                *v = 0.2 * (2.0 * std::f32::consts::PI * params.ding_hz * t).sin();
            } else if (1.09..1.8).contains(&t) {
                *v = 0.15 * (2.0 * std::f32::consts::PI * params.dong_hz * t).sin();
            }
        }

        let (rms_batch, rms_hop) = rms_env_db(&x, sr, 20.0, 5.0);
        let (ding_batch, ding_dt) = band_env_db(&x, sr, params.ding_hz, &params);
        let (dong_batch, dong_dt) = band_env_db(&x, sr, params.dong_hz, &params);

        let mut rms = EnvelopeAccumulator::new(sr, 20.0, 5.0);
        let mut ding = BandEnvelopeAccumulator::new(sr, params.ding_hz, &params);
        let mut dong = BandEnvelopeAccumulator::new(sr, params.dong_hz, &params);
        for chunk in x.chunks(997) {
            rms.push(chunk);
            ding.push(chunk);
            dong.push(chunk);
        }
        let (rms_stream, rms_hop2) = rms.finish();
        let (ding_stream, ding_dt2) = ding.finish();
        let (dong_stream, dong_dt2) = dong.finish();

        assert_eq!(rms_batch, rms_stream);
        assert_eq!(ding_batch, ding_stream);
        assert_eq!(dong_batch, dong_stream);
        assert_eq!(rms_hop, rms_hop2);
        assert_eq!(ding_dt, ding_dt2);
        assert_eq!(dong_dt, dong_dt2);
    }

    /// 宽带瞬态对（间隔 ≈0.59 s）不应被当成叮咚：它没有音调纯度。
    /// 复现「同一材料朗读第二遍处的弱瞬态刚好 Δ≈0.59 s → 骗过旧检测器 → 一份材料被切成两份」。
    #[test]
    fn rejects_broadband_transient_pair() {
        let sr = 32_000u32;
        let mut x = vec![0.0f32; sr as usize * 4];
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0 - 0.5
        };
        let mut burst = |buf: &mut [f32], at_s: f32, amp: f32| {
            let a = (at_s * sr as f32) as usize;
            let n = (0.35 * sr as f32) as usize;
            for i in 0..n.min(buf.len() - a) {
                let t = i as f32 / sr as f32;
                buf[a + i] += amp * (-t / 0.05).exp() * rnd();
            }
        };
        burst(&mut x, 1.0, 1.0);
        burst(&mut x, 1.59, 1.0);
        let chimes = detect_chimes(&x, sr, &ChimeParams::default());
        assert!(
            chimes.is_empty(),
            "宽带瞬态对不应被当成叮咚（Δ≈0.59s 也不行）, got {chimes:?}"
        );
    }
}
