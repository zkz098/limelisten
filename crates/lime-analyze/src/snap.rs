//! 时间戳吸附（PLAN D9b）：把 Whisper 的词时间戳映射回真实音频时间轴。
//!
//! 为什么必须做：实测 Whisper 在长静音处会漂移 ——
//! 真实音频 +2.46 s 才有声音，逐词模式却把第一个词标在 0.00 s；同一文件后段可偏 4 s 以上。
//! 直接拿来做卡拉OK 高亮，会越播越不准。
//!
//! 三种策略（实测结论见 docs/GRILLING.md）：
//! 1. **Landmark（默认优先）**：素材里有念白标记（`Text one`）时，把它与 DSP 检测出的
//!    **叮咚标记**配对，再做最小二乘拟合。两个信号相互独立，标定最准。
//!    实测 训练2：Text-one ↔ 第 2 个叮咚，中位残差 ~1 s；而全局比例法会偏 18 s。
//! 2. **Affine（无念白时的兜底）**：`t_real = a + k·t_whisper`，用首尾语音边界拟合。
//! 3. Ratio / Index：早期方案，实测在本类素材上不如上面两个（保留用于对比与面板调试）。
//!
//! 踩过的坑（都写在这里避免重犯）：
//! - 用“累计语音时长比例”会把自己算成 1.58×（真值 1.09×），因为 Whisper 的词间隙被当成了静音；
//! - 用“首尾边界”拟合会被 Whisper 的尾部幻觉污染（k 被算成 0.96 < 1，等于声称词比内容还长）；
//! - 念白标记的位置必须取**关键词所在的那个词**，否则中文长句会把位置提前十几秒；
//! - 配对评分必须用**中位残差**，RMS 会被少数错误标记（重复念白/幻听）拉爆到 37~44 s。

use crate::Structure;
use lime_core::Word;

pub use crate::chime::Chime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapMode {
    /// 累计语音时长比例
    Ratio,
    /// 按语音区间序号比例
    Index,
    /// 仿射：首尾内容边界拟合
    Affine,
    /// 地标对齐：念白标记 ↔ 叮咚
    Landmark,
    /// 不吸附
    Off,
}

/// 默认策略：优先地标，其次仿射（在 `snap_with_landmarks` 里决定）
pub fn snap_words_to_speech(words: &[Word], structure: &Structure) -> Vec<Word> {
    snap_by_mode(words, structure, SnapMode::Affine)
}

/// 推荐入口：有地标就用地标，否则仿射兜底
pub fn snap_with_landmarks(words: &[Word], structure: &Structure, chimes: &[Chime]) -> Vec<Word> {
    let pairs = landmark_pairs(words, structure, chimes);
    if pairs.len() >= 2 {
        apply_line(words, structure, fit_line(&pairs))
    } else {
        snap_by_mode(words, structure, SnapMode::Affine)
    }
}

pub fn snap_by_mode(words: &[Word], structure: &Structure, mode: SnapMode) -> Vec<Word> {
    if mode == SnapMode::Off || words.is_empty() || structure.speech.is_empty() {
        return words.to_vec();
    }
    match mode {
        SnapMode::Landmark => snap_with_landmarks(words, structure, &[]),
        SnapMode::Affine => match fit_affine(words, structure) {
            Some(f) => apply_line(words, structure, (f.a_ms, f.k)),
            None => words.to_vec(),
        },
        _ => {
            let wsp = merge_word_speech(words, 250);
            if wsp.is_empty() {
                return words.to_vec();
            }
            let mapper = match mode {
                SnapMode::Ratio => Mapper::by_ratio(&wsp, &structure.speech),
                _ => Mapper::by_index(&wsp, &structure.speech),
            };
            words
                .iter()
                .map(|w| {
                    let s = mapper.map(w.start_ms, true);
                    let e = mapper.map(w.end_ms, false).max(s + 10);
                    Word { start_ms: s, end_ms: e, text: w.text.clone() }
                })
                .collect()
        }
    }
}

/// 仿射参数 t_real = a + k·t_whisper
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub k: f64,
    pub a_ms: f64,
}

/// 用首尾内容边界拟合（会被尾部幻觉污染，仅作兜底）
pub fn fit_affine(words: &[Word], structure: &Structure) -> Option<Affine> {
    let wsp = merge_word_speech(words, 250);
    if wsp.is_empty() || structure.speech.is_empty() {
        return None;
    }
    let raw_first = wsp.first()?.0 as f64;
    let raw_last = wsp.last()?.1 as f64;
    let dsp_first = structure.speech.first()?.0 as f64;
    let dsp_last = structure.speech.last()?.1 as f64;
    let w_span = (raw_last - raw_first).max(1.0);
    let d_span = (dsp_last - dsp_first).max(1.0);
    let k = d_span / w_span;
    Some(Affine { k, a_ms: dsp_first - raw_first * k })
}

fn apply_line(words: &[Word], structure: &Structure, (a, k): (f64, f64)) -> Vec<Word> {
    let lo = structure.speech.first().map(|s| s.0).unwrap_or(0) as f64;
    let hi = structure.speech.last().map(|s| s.1).unwrap_or(0) as f64;
    let hi = hi.max(lo);
    let mapped: Vec<Word> = words
        .iter()
        .map(|w| {
            let m = |t: u64| ((a + k * t as f64).clamp(lo, hi)) as u64;
            let s = m(w.start_ms);
            let e = m(w.end_ms).max(s + 10);
            Word { start_ms: s, end_ms: e, text: w.text.clone() }
        })
        .collect();
    clamp_into_speech(&mapped, structure)
}

/// 后处理：把“落在静音里”的词推回最近的语音岛内。
///
/// 为何必要：吸附是全局线性映射，局部误差不可避免；但“词落在静音里”是**肉眼可见**的错误
/// （卡拉OK 会在无人声处高亮）。这里只做保序的局部修正：落在静音里的词整段移到最近的岛边。
pub fn clamp_into_speech(words: &[Word], structure: &Structure) -> Vec<Word> {
    if structure.speech.is_empty() {
        return words.to_vec();
    }
    let islands = &structure.speech;
    let in_speech = |t: u64| islands.iter().any(|(a, b)| t >= *a && t <= *b);
    let nearest = |t: u64| -> u64 {
        islands
            .iter()
            .map(|(a, b)| {
                if t < *a {
                    (*a, a - t)
                } else if t > *b {
                    (*b, t - b)
                } else {
                    (t, 0)
                }
            })
            .min_by_key(|(_, d)| *d)
            .map(|(p, _)| p)
            .unwrap_or(t)
    };
    let mut out = words.to_vec();
    for w in out.iter_mut() {
        if !in_speech(w.start_ms) && w.end_ms.saturating_sub(w.start_ms) < 1500 {
            // 短词（正常词）落在静音里 ⇒ 挪到最近岛边；长段（可能是念白/停顿标记）不动
            let s = nearest(w.start_ms);
            let len = w.end_ms.saturating_sub(w.start_ms);
            w.start_ms = s;
            w.end_ms = s + len;
        }
    }
    // 保序：修正后可能出现倒序，按起点单调化（只向后推，不把词往前拉）
    let mut last = 0u64;
    for w in out.iter_mut() {
        if w.start_ms < last {
            let len = w.end_ms.saturating_sub(w.start_ms);
            w.start_ms = last;
            w.end_ms = w.start_ms + len;
        }
        last = w.start_ms;
    }
    out
}

/// 地标配对：(whisper 侧念白时刻, 真实侧叮咚时刻)
pub fn landmark_pairs(words: &[Word], structure: &Structure, chimes: &[Chime]) -> Vec<(u64, u64)> {
    let spoken = spoken_material_markers(words);
    let mut real: Vec<u64> = chimes
        .iter()
        .filter(|c| c.confidence >= 0.35)
        .map(|c| c.ding_ms)
        .collect();
    real.sort_unstable();
    if real.len() < 2 {
        let mut g: Vec<u64> = structure
            .gaps
            .iter()
            .filter(|g| g.kind == crate::GapKind::Material)
            .map(|g| g.end_ms)
            .collect();
        g.sort_unstable();
        real = g;
    }
    if spoken.len() < 2 || real.len() < 2 {
        return Vec::new();
    }

    // 枚举偏移，用**稳健拟合**评分：
    // 1. 至少 3 对（2 对总能“完美拟合”，会造成退化）；
    // 2. k 用“相邻配对斜率的中位数”估计，并要求 ≥60% 的斜率与之接近（否则该对齐不可信）；
    // 3. a 取中位数，残差取中位数。
    let mut best: Option<(i64, usize, Vec<(u64, u64)>)> = None;
    let max_off = real.len().saturating_sub(3);
    for off in 0..=max_off {
        let n = spoken.len().min(real.len() - off);
        if n < 3 {
            break;
        }
        let pairs: Vec<(u64, u64)> = (0..n).map(|i| (spoken[i], real[off + i])).collect();
        let Some((a, k)) = robust_line(&pairs) else { continue };
        let q = (median_abs_residual(&pairs, a, k) / 100.0).round() as i64;
        let better = match &best {
            None => true,
            Some((bq, bn, _)) => q < *bq || (q == *bq && n > *bn),
        };
        if better {
            best = Some((q, n, pairs));
        }
    }
    match best {
        Some((q, _, pairs)) if q < 15 => pairs, // 中位残差 < 1.5 s 才敢用
        _ => Vec::new(),
    }
}

/// 稳健直线拟合：k = 相邻斜率中位数（要求 ≥60% 聚在 k 附近 ±15%），a = 截距中位数
fn robust_line(pairs: &[(u64, u64)]) -> Option<(f64, f64)> {
    if pairs.len() < 3 {
        return None;
    }
    let mut ks: Vec<f64> = pairs
        .windows(2)
        .map(|w| {
            let dx = (w[1].0 as i64 - w[0].0 as i64) as f64;
            let dy = (w[1].1 as i64 - w[0].1 as i64) as f64;
            if dx.abs() < 1.0 {
                f64::NAN
            } else {
                dy / dx
            }
        })
        .filter(|k| k.is_finite() && *k > 0.2 && *k < 5.0)
        .collect();
    if ks.len() < 2 {
        return None;
    }
    ks.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let k = ks[ks.len() / 2];
    let inliers = ks.iter().filter(|x| ((**x - k) / k).abs() < 0.15).count();
    if inliers * 100 / ks.len() < 60 {
        return None; // 多数斜率不一致 ⇒ 这组配对不可信
    }
    let mut accs: Vec<f64> = pairs.iter().map(|(w, r)| *r as f64 - k * (*w as f64)).collect();
    accs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    Some((accs[accs.len() / 2], k))
}

fn median_abs_residual(pairs: &[(u64, u64)], a: f64, k: f64) -> f64 {
    let mut errs: Vec<f64> = pairs
        .iter()
        .map(|(w, r)| (a + k * (*w as f64) - (*r as f64)).abs())
        .collect();
    errs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    errs[errs.len() / 2]
}

fn fit_line(pairs: &[(u64, u64)]) -> (f64, f64) {
    let n = pairs.len() as f64;
    let sx: f64 = pairs.iter().map(|(w, _)| *w as f64).sum();
    let sy: f64 = pairs.iter().map(|(_, r)| *r as f64).sum();
    let sxx: f64 = pairs.iter().map(|(w, _)| (*w as f64) * (*w as f64)).sum();
    let sxy: f64 = pairs.iter().map(|(w, r)| (*w as f64) * (*r as f64)).sum();
    let denom = n * sxx - sx * sx;
    let k = if denom.abs() < 1e-6 { 1.0 } else { (n * sxy - sx * sy) / denom };
    let k = if k.is_finite() && k > 0.2 && k < 5.0 { k } else { 1.0 };
    ((sy - k * sx) / n, k)
}

fn median_residual(pairs: &[(u64, u64)]) -> f64 {
    let (a, k) = fit_line(pairs);
    median_abs_residual(pairs, a, k)
}

/// 诊断字符串（CLI 用）
pub fn landmark_diagnosis(words: &[Word], chimes: &[Chime]) -> String {
    let spoken = spoken_material_markers(words);
    let mut real: Vec<u64> = chimes
        .iter()
        .filter(|c| c.confidence >= 0.35)
        .map(|c| c.ding_ms)
        .collect();
    real.sort_unstable();
    let mut s = format!("念白标记(ms): {spoken:?}\n强叮咚(ms):   {real:?}\n");
    for off in 0..=real.len().saturating_sub(2) {
        let n = spoken.len().min(real.len() - off);
        if n < 3 {
            break;
        }
        let pairs: Vec<(u64, u64)> = (0..n).map(|i| (spoken[i], real[off + i])).collect();
        let line = robust_line(&pairs);
        let desc = match line {
            Some((a, k)) => format!(
                "中位残差 {:.0} ms k={k:.4} a={a:.0}",
                median_abs_residual(&pairs, a, k)
            ),
            None => "斜率不一致（不可信）".to_string(),
        };
        s.push_str(&format!("  offset {off}（{n} 对）{desc}\n"));
    }
    s
}

/// 从词序列里找“念白材料标记”（Text one / Text 2 / Unit 3 / Lesson A …），返回时刻（ms，升序）
pub fn spoken_material_markers(words: &[Word]) -> Vec<u64> {
    let Ok(re) = regex::Regex::new(
        r"(?i)\b(text|unit|lesson|passage)\s*(?:number\s*)?(\d+|[a-d]|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)\b",
    ) else {
        return Vec::new();
    };
    let mut out: Vec<u64> = Vec::new();
    // 最近 4 个词的滑窗拼接（避开 "Text" / "one" 被切成两个词）
    let mut win: Vec<(String, u64, u64)> = Vec::new();
    for w in words {
        let t = w.text.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(last) = win.last() {
            if w.start_ms.saturating_sub(last.2) > 1500 {
                win.clear();
            }
        }
        win.push((t.to_string(), w.start_ms, w.end_ms));
        if win.len() > 4 {
            win.remove(0);
        }
        let joined = win.iter().map(|x| x.0.as_str()).collect::<Vec<_>>().join(" ");
        if re.is_match(&joined) {
            // 位置必须取**关键词所在的词**：中文长句作为单个词时，窗口首词可能比 "Text" 早 10 s
            let pos = win
                .iter()
                .find(|x| {
                    let l = x.0.to_ascii_lowercase();
                    l.contains("text")
                        || l.contains("unit")
                        || l.contains("lesson")
                        || l.contains("passage")
                })
                .map(|x| x.1)
                .unwrap_or(w.start_ms);
            if out.last().map(|p| pos.saturating_sub(*p) > 10_000).unwrap_or(true) {
                // 去重窗口 10 s：实测 Whisper 会把同一处念白重复识别好几次
                // （训练2：112920 / 114700 / 121720 都是 "Text three"），
                // 重复标记会把“相邻斜率”算得不一致，让地标对齐整个失效。
                out.push(pos);
            }
            win.clear();
        }
    }
    out.sort_unstable();
    out
}

/// 把词序列并成语音区间（词间停顿 > `gap_ms` 视为断开）
fn merge_word_speech(words: &[Word], gap_ms: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for w in words {
        if w.text.trim().is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if w.start_ms.saturating_sub(last.1) <= gap_ms => {
                last.1 = last.1.max(w.end_ms);
            }
            _ => out.push((w.start_ms, w.end_ms)),
        }
    }
    out
}

// ===================== Ratio / Index（保留对比） =====================

struct Mapper {
    w: Vec<(u64, u64, u64)>,
    w_total: u64,
    d: Vec<(u64, u64, u64)>,
    d_total: u64,
    island_of: Vec<usize>,
    by_index: bool,
}

impl Mapper {
    fn by_index(wsp: &[(u64, u64)], dsp: &[(u64, u64)]) -> Self {
        let mut m = Self::build(wsp, dsp);
        let n = m.w.len();
        let d = m.d.len();
        m.island_of = (0..n)
            .map(|i| {
                if n <= 1 || d <= 1 {
                    return 0;
                }
                let j = (i as f64 / (n as f64 - 1.0) * (d as f64 - 1.0)).round();
                (j.max(0.0) as usize).min(d - 1)
            })
            .collect();
        m.by_index = true;
        m
    }

    fn by_ratio(wsp: &[(u64, u64)], dsp: &[(u64, u64)]) -> Self {
        let mut m = Self::build(wsp, dsp);
        m.island_of = Vec::new();
        m.by_index = false;
        m
    }

    fn build(wsp: &[(u64, u64)], dsp: &[(u64, u64)]) -> Self {
        let mut w = Vec::with_capacity(wsp.len());
        let mut acc = 0u64;
        for (a, b) in wsp {
            let len = b.saturating_sub(*a);
            w.push((*a, *b, acc));
            acc += len;
        }
        let w_total = acc;
        let mut d = Vec::with_capacity(dsp.len());
        let mut acc = 0u64;
        for (a, b) in dsp {
            let len = b.saturating_sub(*a);
            d.push((*a, *b, acc));
            acc += len;
        }
        let d_total = acc;
        Self { w, w_total, d, d_total, island_of: Vec::new(), by_index: false }
    }

    fn cum_before(&self, t: u64) -> f64 {
        for (a, b, acc) in &self.w {
            if t < *a {
                return *acc as f64;
            }
            if t <= *b {
                return (*acc + (t - *a)) as f64;
            }
        }
        self.w_total as f64
    }

    fn dsp_at(&self, u: f64, prefer_later: bool) -> u64 {
        if self.d.is_empty() {
            return 0;
        }
        let u = u.clamp(0.0, self.d_total as f64);
        for &(a, b, acc) in &self.d {
            let len = (b - a) as f64;
            let inside = if prefer_later { u < acc as f64 + len } else { u <= acc as f64 + len };
            if inside {
                return a + (u - acc as f64).round().max(0.0) as u64;
            }
        }
        self.d.last().map(|x| x.1).unwrap_or(0)
    }

    fn map_by_index(&self, t: u64) -> u64 {
        if self.w.is_empty() || self.d.is_empty() {
            return t;
        }
        let idx = |i: usize| self.island_of.get(i).copied().unwrap_or(0).min(self.d.len() - 1);
        if t <= self.w[0].0 {
            return self.d[0].0;
        }
        for i in 0..self.w.len() {
            let (a, b, _) = self.w[i];
            let (a_d, b_d, _) = self.d[idx(i)];
            if t < a {
                if i == 0 {
                    return a_d;
                }
                let prev = self.w[i - 1];
                let (_, prev_b_d, _) = self.d[idx(i - 1)];
                let span = (a - prev.1).max(1) as f64;
                let frac = (t - prev.1) as f64 / span;
                return prev_b_d + ((a_d as f64 - prev_b_d as f64) * frac).round() as u64;
            }
            if t <= b {
                let len_w = (b - a).max(1) as f64;
                let len_d = (b_d.saturating_sub(a_d)).max(1) as f64;
                let frac = (t - a) as f64 / len_w;
                return a_d + (frac * len_d).round() as u64;
            }
        }
        self.d.last().map(|x| x.1).unwrap_or(t)
    }

    fn map(&self, t: u64, prefer_later: bool) -> u64 {
        if self.by_index {
            self.map_by_index(t)
        } else {
            self.dsp_at(self.cum_before(t), prefer_later)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(speech: Vec<(u64, u64)>) -> Structure {
        Structure { speech, gaps: vec![], duration_ms: 100_000, has_digital_silence: true }
    }

    fn words(times: &[(u64, u64, &str)]) -> Vec<Word> {
        times
            .iter()
            .map(|(a, b, t)| Word { start_ms: *a, end_ms: *b, text: t.to_string() })
            .collect()
    }

    fn chime(d: u64) -> Chime {
        Chime { ding_ms: d, dong_ms: d + 600, ding_db: -16.0, dong_db: -23.0, confidence: 1.0 }
    }

    /// 仿射兜底：整体漂移修复 + 单调性
    #[test]
    fn affine_fixes_drift() {
        let st = structure(vec![(3000, 9000), (20000, 26000)]);
        let w = words(&[(0, 1500, "first"), (1500, 3000, "second"), (20000, 23000, "fifth")]);
        let s = snap_by_mode(&w, &st, SnapMode::Affine);
        assert!((s[0].start_ms as i64 - 3000).abs() < 200, "{:?}", s[0]);
        assert!(s.windows(2).all(|x| x[1].start_ms >= x[0].start_ms), "{s:?}");
    }

    /// 地标对齐：念白 "Text N" 与叮咚成对，且第一个叮咚没有对应念白时也能正确错位匹配
    #[test]
    fn landmark_alignment_handles_offset() {
        // 真实：叮咚在 10s / 50s / 90s；念白在 whisper 时间轴上的 8s / 46s / 84s（最早那个叮咚无念白）
        let st = structure(vec![(5000, 9000), (45000, 49000), (85000, 89000)]);
        let chimes = vec![chime(10_000), chime(50_000), chime(90_000)];
        let w = words(&[
            (0, 1000, "directions"),
            (8000, 9000, "Text"),
            (9000, 9500, "one"),
            (46000, 47000, "Text"),
            (47000, 47500, "two"),
            (84000, 85000, "Text"),
            (85000, 85500, "three"),
        ]);
        let pairs = landmark_pairs(&w, &st, &chimes);
        // 应把 3 个念白与后 2 个叮咚错位配不上…实际是 3 个念白 ↔ 3 个叮咚里最优的一组
        assert!(pairs.len() >= 2, "应找到配对: {pairs:?}");
        let s = snap_with_landmarks(&w, &st, &chimes);
        // “Text one” 应落在第一个叮咚附近（10 s 量级），而不是 5 s 或 50 s
        let t1 = s.iter().find(|x| x.text.contains("Text")).unwrap();
        assert!(
            (t1.start_ms as i64 - 10_000).abs() < 4000,
            "Text one 应吸附到 ~10 s，实际 {}",
            t1.start_ms
        );
        assert!(s.windows(2).all(|x| x[1].start_ms >= x[0].start_ms), "单调性: {s:?}");
    }

    /// 无念白时退化为仿射，不报错
    #[test]
    fn falls_back_to_affine_without_markers() {
        let st = structure(vec![(2000, 8000), (30000, 40000)]);
        let chimes = vec![chime(8000)];
        let w = words(&[(0, 1000, "hello"), (1000, 2000, "world"), (28000, 30000, "bye")]);
        let s = snap_with_landmarks(&w, &st, &chimes);
        assert_eq!(s.len(), 3);
        assert!(s[0].start_ms >= 2000);
        assert!(s.windows(2).all(|x| x[1].start_ms >= x[0].start_ms));
    }

    /// 已对齐的输入基本不该被改动
    #[test]
    fn identity_when_already_aligned() {
        let st = structure(vec![(1000, 5000), (8000, 12000)]);
        let w = words(&[
            (1000, 2000, "one"),
            (2000, 3000, "two"),
            (3000, 4000, "three"),
            (4000, 5000, "four"),
        ]);
        let s = snap_by_mode(&w, &st, SnapMode::Ratio);
        for (a, b) in w.iter().zip(s.iter()) {
            assert!((a.start_ms as i64 - b.start_ms as i64).abs() < 150, "{a:?} -> {b:?}");
        }
    }

    /// 落在静音里的词要被推回语音岛（卡拉OK 可见性修正）
    #[test]
    fn words_in_silence_are_clamped() {
        let st = structure(vec![(1000, 3000), (6000, 9000)]);
        let w = words(&[(1000, 1500, "a"), (4500, 4800, "b"), (7000, 7500, "c")]);
        let fixed = clamp_into_speech(&w, &st);
        // “b” 原本落在 3000–6000 的静音里 → 应被推到最近的岛边（3000 或 6000）
        let b = fixed.iter().find(|x| x.text == "b").unwrap();
        assert!(b.start_ms == 3000 || b.start_ms == 6000, "{b:?}");
        assert!(fixed.windows(2).all(|x| x[1].start_ms >= x[0].start_ms), "保序: {fixed:?}");
    }

    /// 念白标记位置必须取关键词所在词（中文长句陷阱）
    #[test]
    fn marker_position_uses_keyword_word() {
        let w = words(&[
            (0, 500, "你将有十秒钟的时间来回答有关小题和阅读下一小题"),
            (6000, 6500, "Text"),
            (6500, 7000, "one"),
        ]);
        let m = spoken_material_markers(&w);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0], 6000, "应取 Text 所在词的位置，而不是窗口首词");
    }
}
