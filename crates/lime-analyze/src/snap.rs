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

/// 安全词时间戳校准：
/// 1. 修正起始静音：若首词 start_ms == 0，而真实首个语音岛在 t0 (如 3056 ms) 才开始，将首词平移至 t0；
/// 2. 保持真实物理语速 1.0x，保留 Whisper 原生 10ms 帧精度（严禁将人声强行吸附到叮咚铃声，杜绝提前 1~3 秒错误）；
/// 3. 严格单调递增，杜绝时钟倒流。
pub fn align_words_safe(words: &[Word], structure: &Structure) -> Vec<Word> {
    if words.is_empty() {
        return Vec::new();
    }
    let mut out = words.to_vec();
    if let Some(&(first_speech_start, _)) = structure.speech.first() {
        if out[0].start_ms == 0 && first_speech_start > 500 {
            let offset = first_speech_start;
            out[0].start_ms = offset;
            out[0].end_ms = out[0].end_ms.max(offset + 100);
        }
    }
    let mut last_end = 0u64;
    for w in out.iter_mut() {
        if w.start_ms < last_end {
            let len = w.end_ms.saturating_sub(w.start_ms).max(50);
            w.start_ms = last_end;
            w.end_ms = w.start_ms + len;
        }
        last_end = w.start_ms;
    }
    out
}

/// 默认策略：保持 Whisper 高精度原生时间戳，校准开头静音
pub fn snap_words_to_speech(words: &[Word], structure: &Structure) -> Vec<Word> {
    align_words_safe(words, structure)
}

/// 推荐入口：校准时间戳，消除误配叮咚产生的 1~3 秒人为提前漂移
pub fn snap_with_landmarks(words: &[Word], structure: &Structure, _chimes: &[Chime]) -> Vec<Word> {
    align_words_safe(words, structure)
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

/// 分段线性插值映射：基于匹配的地标点序列进行逐段对齐，两端平滑外推
pub fn apply_piecewise_linear(
    words: &[Word],
    structure: &Structure,
    pairs: &[(u64, u64)],
) -> Vec<Word> {
    if pairs.len() < 2 {
        return words.to_vec();
    }
    let lo = structure.speech.first().map(|s| s.0).unwrap_or(0) as f64;
    let hi = structure.speech.last().map(|s| s.1).unwrap_or(0) as f64;
    let hi = hi.max(lo);

    let k_first = {
        let (x0, y0) = pairs[0];
        let (x1, y1) = pairs[1];
        let dx = (x1 as f64 - x0 as f64).max(1.0);
        ((y1 as f64 - y0 as f64) / dx).clamp(0.5, 2.0)
    };
    let k_last = {
        let n = pairs.len();
        let (x0, y0) = pairs[n - 2];
        let (x1, y1) = pairs[n - 1];
        let dx = (x1 as f64 - x0 as f64).max(1.0);
        ((y1 as f64 - y0 as f64) / dx).clamp(0.5, 2.0)
    };

    let map_time = |t: u64| -> u64 {
        let t_f = t as f64;
        let mapped = if t <= pairs[0].0 {
            // 前端外推
            pairs[0].1 as f64 - k_first * (pairs[0].0 as f64 - t_f)
        } else if t >= pairs.last().unwrap().0 {
            // 后端外推
            pairs.last().unwrap().1 as f64 + k_last * (t_f - pairs.last().unwrap().0 as f64)
        } else {
            // 找到区间 pairs[i].0 <= t <= pairs[i+1].0
            let mut val = t_f;
            for i in 0..pairs.len() - 1 {
                let (x0, y0) = pairs[i];
                let (x1, y1) = pairs[i + 1];
                if t >= x0 && t <= x1 {
                    let dx = (x1 as f64 - x0 as f64).max(1.0);
                    let frac = (t_f - x0 as f64) / dx;
                    val = y0 as f64 + frac * (y1 as f64 - y0 as f64);
                    break;
                }
            }
            val
        };
        mapped.clamp(lo, hi).round().max(0.0) as u64
    };

    let mapped: Vec<Word> = words
        .iter()
        .map(|w| {
            let s = map_time(w.start_ms);
            let e = map_time(w.end_ms).max(s + 10);
            Word { start_ms: s, end_ms: e, text: w.text.clone() }
        })
        .collect();
    clamp_into_speech(&mapped, structure)
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
    if spoken.is_empty() || real.is_empty() {
        return Vec::new();
    }

    align_markers_dtw(&spoken, &real, words, structure)
}

/// 单调序列动态规划对齐（Needleman-Wunsch / DTW 变种）：
/// 寻找 spoken 与 real 的单调子序列匹配，允许跳过叮咚/念白，同时强制局部斜率在 [0.65, 1.6] 合理物理区间内。
fn align_markers_dtw(
    spoken: &[u64],
    real: &[u64],
    words: &[Word],
    structure: &Structure,
) -> Vec<(u64, u64)> {
    let m = spoken.len();
    let n = real.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }

    // 粗粒度基线预测器（首尾仿射）
    let (a_base, k_base) = match fit_affine(words, structure) {
        Some(f) if f.k > 0.5 && f.k < 2.0 => (f.a_ms, f.k),
        _ => (0.0, 1.0),
    };
    let predict = |s: u64| -> f64 { a_base + k_base * s as f64 };

    const SKIP_S_COST: f64 = 6000.0;
    const SKIP_R_COST: f64 = 3000.0;
    const MAX_ABS_DIFF: f64 = 25000.0; // 允许的最大漂移 25 秒

    // dp[i][j] = 以 (spoken[i], real[j]) 作为匹配对时的最小累计代价
    // parent[i][j] = 上一个匹配对的坐标 (i', j')，或 None
    let mut dp = vec![vec![f64::INFINITY; n]; m];
    let mut parent = vec![vec![None; n]; m];

    for i in 0..m {
        for j in 0..n {
            let diff = (predict(spoken[i]) - real[j] as f64).abs();
            if diff > MAX_ABS_DIFF {
                continue;
            }
            let match_cost = diff;

            // 情况 1: (i, j) 作为首个匹配对
            let init_cost = (i as f64) * SKIP_S_COST + (j as f64) * SKIP_R_COST + match_cost;
            dp[i][j] = init_cost;

            // 情况 2: 从之前的某个匹配对 (pi, pj) 转移过来
            for pi in 0..i {
                for pj in 0..j {
                    let prev_cost = dp[pi][pj];
                    if !prev_cost.is_finite() {
                        continue;
                    }
                    let dx = (spoken[i] as i64 - spoken[pi] as i64) as f64;
                    let dy = (real[j] as i64 - real[pj] as i64) as f64;
                    if dx < 100.0 || dy < 100.0 {
                        continue;
                    }
                    let slope = dy / dx;
                    // 局部斜率约束：必须符合合理的语速/静音伸缩范围
                    if slope >= 0.65 && slope <= 1.6 {
                        let skipped_s = (i - pi - 1) as f64 * SKIP_S_COST;
                        let skipped_r = (j - pj - 1) as f64 * SKIP_R_COST;
                        let cost = prev_cost + skipped_s + skipped_r + match_cost;
                        if cost < dp[i][j] {
                            dp[i][j] = cost;
                            parent[i][j] = Some((pi, pj));
                        }
                    }
                }
            }
        }
    }

    // 寻找以任意 (i, j) 结尾加上尾部 skip 惩罚后的全局最优解
    let mut best_total = f64::INFINITY;
    let mut best_end = None;

    for i in 0..m {
        for j in 0..n {
            let c = dp[i][j];
            if c.is_finite() {
                let tail_cost = (m - 1 - i) as f64 * SKIP_S_COST + (n - 1 - j) as f64 * SKIP_R_COST;
                let total = c + tail_cost;
                if total < best_total {
                    best_total = total;
                    best_end = Some((i, j));
                }
            }
        }
    }

    let Some(mut cur) = best_end else {
        return Vec::new();
    };

    let mut path = Vec::new();
    loop {
        path.push((spoken[cur.0], real[cur.1]));
        if let Some(prev) = parent[cur.0][cur.1] {
            cur = prev;
        } else {
            break;
        }
    }
    path.reverse();

    if path.len() >= 2 {
        path
    } else {
        Vec::new()
    }
}

/// 诊断字符串（CLI 用）
pub fn landmark_diagnosis(words: &[Word], structure: &Structure, chimes: &[Chime]) -> String {
    let spoken = spoken_material_markers(words);
    let mut real: Vec<u64> = chimes
        .iter()
        .filter(|c| c.confidence >= 0.35)
        .map(|c| c.ding_ms)
        .collect();
    real.sort_unstable();
    let mut s = format!("念白标记(ms): {spoken:?}\n强叮咚(ms):   {real:?}\n");
    let pairs = landmark_pairs(words, structure, chimes);
    if pairs.is_empty() {
        s.push_str("  DTW单调对齐未找到有效配对（将回退为仿射）\n");
    } else {
        s.push_str(&format!("  DTW单调对齐成功（{} 对）：\n", pairs.len()));
        for (w, r) in &pairs {
            let diff = *r as i64 - *w as i64;
            s.push_str(&format!(
                "    whisper {:>9} ms  ->  真实 {:>9} ms (Δ = {:+6} ms)\n",
                w, r, diff
            ));
        }
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
