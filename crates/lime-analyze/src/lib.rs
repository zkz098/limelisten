//! 结构分析：能量包络 → 精确静音 → 语音岛 → 间隙分类 → 叮咚标记 → 两级章节骨架。
//!
//! 依据 `docs/GRILLING.md` §2（全部在 4 个真实素材上实测标定）：
//! - 本类素材含**精确数字静音**（采样值恒 0，占全长 22.9%），结构检测是最可靠的骨架来源；
//! - **叮咚标记**（叮 2578 Hz → 咚 2039 Hz，Δ≈0.59 s）出现在答题间隔末尾，即材料分界处，
//!   与静音结构互相独立、互相印证 —— 两者一致时置信度最高；
//! - 纯"嘀声"式持续音调检测被实测否决（单文件误报 475–758 个），故不采用。

use lime_core::{normalize_chapters, Chapter, ChapterLevel, ChapterSource};

pub mod chime;
pub mod snap;

pub use chime::{detect_chimes, Chime, ChimeParams};

/// 分析参数（可在参数面板里调，用于"只给参数面板重跑"的修正路径）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyzeParams {
    pub frame_ms: f32,
    pub hop_ms: f32,
    /// 低于此 dBFS 视为静音（-55 dBFS 依据实测）
    pub silence_db: f32,
    /// 句内/句间停顿下限（秒）
    pub gap_sentence_s: f32,
    /// 题边界下限（你选定：2–8 s 间隙也算题界）
    pub gap_question_s: f32,
    /// 材料边界下限（无叮咚可依赖时的兜底；有叮咚时以叮咚为准）
    pub gap_material_s: f32,
    /// 边界吸附半径（秒）
    pub snap_s: f32,
    pub chime: ChimeParams,
}

impl Default for AnalyzeParams {
    fn default() -> Self {
        Self {
            frame_ms: 20.0,
            hop_ms: 5.0,
            silence_db: -55.0,
            gap_sentence_s: 0.35,
            gap_question_s: 2.0,
            gap_material_s: 8.0,
            snap_s: 0.4,
            chime: ChimeParams::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    /// 句内停屯/句间边界
    Sentence,
    /// 题边界
    Question,
    /// 材料边界（答题间隔；实测 5.4–20 s）
    Material,
}

/// 题级分段的最小长度（毫秒）：短于此值的"题"并进上一题。
///
/// 依据：数字静音在材料末尾残留几毫秒时，`GapKind::Question` 会贴出一个几乎重合的
/// 边界，旧实现会切出 `00:12.7 – 00:12.7` 这种 0–40 ms 的空题（实测 `训练1`/`训练2`
/// 各命中一处），在左侧导航里表现为"点了没反应的题"。
pub const MIN_QUESTION_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    pub start_ms: u64,
    pub end_ms: u64,
    pub kind: GapKind,
}

impl Gap {
    pub fn duration_s(&self) -> f32 {
        (self.end_ms.saturating_sub(self.start_ms)) as f32 / 1000.0
    }
}

/// 10 ms 级能量包络（dBFS）。
pub fn energy_envelope(samples: &[f32], sample_rate: u32, params: &AnalyzeParams) -> Vec<f32> {
    let frame = ((params.frame_ms / 1000.0) * sample_rate as f32).round().max(1.0) as usize;
    let hop = ((params.hop_ms / 1000.0) * sample_rate as f32).round().max(1.0) as usize;
    if samples.len() < frame {
        return Vec::new();
    }
    let n = 1 + (samples.len() - frame) / hop;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let seg = &samples[i * hop..i * hop + frame];
        let mean_sq = seg.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / frame as f64;
        out.push((10.0 * (mean_sq + 1e-20).log10()) as f32);
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Structure {
    /// 语音岛 (start_ms, end_ms)
    pub speech: Vec<(u64, u64)>,
    pub gaps: Vec<Gap>,
    pub duration_ms: u64,
    /// 是否含有精确数字静音（样值恒 0）—— 高置信结构信号
    pub has_digital_silence: bool,
}

fn classify(dur_s: f32, p: &AnalyzeParams) -> Option<GapKind> {
    if dur_s >= p.gap_material_s {
        Some(GapKind::Material)
    } else if dur_s >= p.gap_question_s {
        Some(GapKind::Question)
    } else if dur_s >= p.gap_sentence_s {
        Some(GapKind::Sentence)
    } else {
        None
    }
}

/// 由能量包络得到语音岛与间隙分类。
///
/// 注意：帧移按采样点取整，所以真实帧移 ≠ 名义 `params.hop_ms`
/// （例：44.1 kHz 下 5 ms = 220.5 → 221 样本 = 5.0113 ms）。
/// 这里用**真实帧移**做下标→时间换算；否则 932 s 的文件末尾会漂移约 2 s。
pub fn detect_structure(env: &[f32], sample_rate: u32, params: &AnalyzeParams) -> Structure {
    let hop_samples = ((params.hop_ms / 1000.0) * sample_rate as f32).round().max(1.0);
    let hop_ms = hop_samples as f64 / sample_rate as f64 * 1000.0;
    let total_ms = (env.len() as f64 * hop_ms) as u64;
    let mut speech = Vec::new();
    let mut gaps = Vec::new();
    let mut i = 0usize;
    while i < env.len() {
        if env[i] > params.silence_db {
            let start = i;
            let mut j = i;
            while j + 1 < env.len() && env[j + 1] > params.silence_db {
                j += 1;
            }
            speech.push(((start as f64 * hop_ms) as u64, ((j + 1) as f64 * hop_ms) as u64));
            i = j + 1;
        } else {
            let start = i;
            let mut j = i;
            while j + 1 < env.len() && env[j + 1] <= params.silence_db {
                j += 1;
            }
            let s_ms = (start as f64 * hop_ms) as u64;
            let e_ms = ((j + 1) as f64 * hop_ms) as u64;
            if let Some(kind) = classify((e_ms - s_ms) as f32 / 1000.0, params) {
                gaps.push(Gap { start_ms: s_ms, end_ms: e_ms, kind });
            }
            i = j + 1;
        }
    }
    Structure { speech, gaps, duration_ms: total_ms, has_digital_silence: false }
}

/// 数字静音检测：连续样值恒为 0 的区间（实测本类素材存在，是极强结构信号）。
pub fn digital_silence_spans(samples: &[f32], sample_rate: u32, min_ms: u64) -> Vec<(u64, u64)> {
    let min_len = (min_ms as f64 / 1000.0 * sample_rate as f64) as usize;
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < samples.len() {
        if samples[i] == 0.0 {
            let start = i;
            let mut j = i;
            while j + 1 < samples.len() && samples[j + 1] == 0.0 {
                j += 1;
            }
            if j - start + 1 >= min_len.max(1) {
                out.push((
                    (start as f64 / sample_rate as f64 * 1000.0) as u64,
                    ((j + 1) as f64 / sample_rate as f64 * 1000.0) as u64,
                ));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// ASR 语义锚点（**仅作命名提示**；实测 `(bell chimes)` 是数字静音上的幻觉，故默认不启用）。
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub time_ms: u64,
    pub kind: AnchorKind,
    pub capture: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// 念白材料编号 `Text 1` / `Unit 2` / `Lesson 3`（本批素材未出现）
    SpokenMaterial,
    /// 念白题号 `Question 1` / `第 1 题`（本批素材未出现）
    SpokenQuestion,
}

impl Anchor {
    pub fn is_material_boundary(&self) -> bool {
        matches!(self.kind, AnchorKind::SpokenMaterial)
    }
    pub fn is_question_boundary(&self) -> bool {
        matches!(self.kind, AnchorKind::SpokenQuestion)
    }
    pub fn title_hint(&self) -> Option<String> {
        self.capture.as_ref().map(|c| match self.kind {
            AnchorKind::SpokenMaterial => format!("Text {}", c),
            AnchorKind::SpokenQuestion => format!("第 {} 题", c),
        })
    }
}

/// 一个材料分界点：可能由静音结构、叮咚标记、或两者共同给出。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Boundary {
    /// 上一段内容的结束（答题间隔的起点）
    pub content_end_ms: u64,
    /// 下一段内容的开始（答题间隔的结束 / 咚 结束处）
    pub next_start_ms: u64,
    pub from_gap: bool,
    pub from_chime: bool,
    pub confidence: f32,
}

/// 收集并合并材料分界点：静音间隙 与 叮咚标记 相邻时合并，置信度最高。
pub fn collect_boundaries(
    structure: &Structure,
    chimes: &[Chime],
    params: &AnalyzeParams,
) -> Vec<Boundary> {
    let mut out: Vec<Boundary> = Vec::new();
    // 只用够可信的叮咚参与切分（弱候选保留在报告里，不污染章节）
    let chimes: Vec<Chime> = chimes
        .iter()
        .copied()
        .filter(|c| c.confidence >= params.chime.min_confidence)
        .collect();
    let mut chime_used = vec![false; chimes.len()];

    for g in structure.gaps.iter().filter(|g| g.kind == GapKind::Material) {
        // 找落在该间隙附近（间隙内 或 间隙结束后 1.5 s 内）的叮咚
        let mut hit: Option<usize> = None;
        for (i, c) in chimes.iter().enumerate() {            if chime_used[i] {
                continue;
            }
            let d = c.ding_ms as i64;
            if d >= g.start_ms as i64 - 300 && d <= g.end_ms as i64 + 1500 {
                hit = Some(i);
                break;
            }
        }
        match hit {
            Some(i) => {
                chime_used[i] = true;
                let c = chimes[i];
                out.push(Boundary {
                    content_end_ms: g.start_ms,
                    next_start_ms: g.end_ms.max(c.content_start_ms()),
                    from_gap: true,
                    from_chime: true,
                    confidence: 0.98,
                });
            }
            None => out.push(Boundary {
                content_end_ms: g.start_ms,
                next_start_ms: g.end_ms,
                from_gap: true,
                from_chime: false,
                confidence: 0.85,
            }),
        }
    }

    // 没被任何间隙"认领"的叮咚：自己就是一个分界点（说明静音结构漏了、或间隔没到阈值）
    for (i, c) in chimes.iter().enumerate() {
        if chime_used[i] {
            continue;
        }
        out.push(Boundary {
            content_end_ms: c.ding_ms,
            next_start_ms: c.content_start_ms(),
            from_gap: false,
            from_chime: true,
            confidence: 0.75 * c.confidence.max(0.4) + 0.2,
        });
    }

    out.sort_by_key(|b| b.next_start_ms);
    // 合并过近的分界点（<1.5 s）
    let mut merged: Vec<Boundary> = Vec::new();
    for b in out {
        if let Some(last) = merged.last_mut() {
            if b.next_start_ms.saturating_sub(last.next_start_ms) < 1500 {
                last.content_end_ms = last.content_end_ms.min(b.content_end_ms);
                last.next_start_ms = last.next_start_ms.max(b.next_start_ms);
                last.from_gap |= b.from_gap;
                last.from_chime |= b.from_chime;
                last.confidence = last.confidence.max(b.confidence);
                continue;
            }
        }
        merged.push(b);
    }
    merged
}

fn snap_to_silence(t: u64, structure: &Structure, radius_ms: u64) -> u64 {
    let lo = t.saturating_sub(radius_ms);
    let hi = t + radius_ms;
    structure
        .gaps
        .iter()
        .filter(|g| g.start_ms <= hi && g.end_ms >= lo)
        .min_by_key(|g| g.start_ms.abs_diff(t))
        .map(|g| g.end_ms)
        .unwrap_or(t)
}

/// 由结构 + 叮咚 + 锚点构建两级章节（**材料级**为主，题级按 `gap_question_s` 细分）。
pub fn build_chapters(
    structure: &Structure,
    anchors: &[Anchor],
    chimes: &[Chime],
    params: &AnalyzeParams,
) -> Vec<Chapter> {
    let boundaries = collect_boundaries(structure, chimes, params);
    let snap_ms = (params.snap_s * 1000.0) as u64;

    // 材料区间：以分界点为界，首尾由实际语音裁掉空白
    let first_speech = structure.speech.first().map(|s| s.0).unwrap_or(0);
    let last_speech = structure.speech.last().map(|s| s.1).unwrap_or(structure.duration_ms);

    let mut ranges: Vec<(u64, u64, f32, bool, bool)> = Vec::new();
    let mut cursor = first_speech;
    for b in &boundaries {
        let end = b.content_end_ms.max(cursor);
        if end > cursor {
            ranges.push((cursor, end, b.confidence, b.from_gap, b.from_chime));
        }
        cursor = b.next_start_ms.max(cursor);
    }
    if last_speech > cursor {
        ranges.push((cursor, last_speech, 0.9, false, false));
    }

    let mut out = Vec::new();
    for (m_idx, (start, end, conf, from_gap, from_chime)) in ranges.iter().enumerate() {
        let mat_idx = out.len();
        // 首份材料按产品口径就是「引言」（开考提示/试音/说明段），不参与材料编号；
        // 其余材料保留 `材料 N` 自动名，由 `normalize_chapters` 统一顺延编号。
        let auto_title = if m_idx == 0 {
            lime_core::INTRO_TITLE.to_string()
        } else {
            format!("材料 {}", m_idx + 1)
        };
        let mat_title = if m_idx == 0 {
            auto_title
        } else {
            anchors
                .iter()
                .filter(|a| a.time_ms >= *start && a.time_ms < end + snap_ms)
                .find_map(|a| a.title_hint())
                .unwrap_or(auto_title)
        };
        out.push(Chapter {
            seq: out.len() as u32,
            level: ChapterLevel::Material,
            parent: None,
            ordinal: m_idx as u32 + 1,
            start_ms: *start,
            end_ms: *end,
            title: mat_title,
            source: if *from_chime { ChapterSource::Asr } else { ChapterSource::Structure },
            confidence: *conf,
            locked: false,
        });
        let _ = from_gap;

        // 材料内的题边界：2 s ≤ 间隙 < 8 s（你选定的口径）
        let mut q_bounds: Vec<u64> = structure
            .gaps
            .iter()
            .filter(|g| {
                g.kind == GapKind::Question && g.end_ms > *start && g.start_ms < *end
            })
            .map(|g| g.end_ms.min(*end))
            .collect();
        for a in anchors.iter().filter(|a| a.is_question_boundary()) {
            if a.time_ms > *start && a.time_ms < *end {
                q_bounds.push(snap_to_silence(a.time_ms, structure, snap_ms));
            }
        }
        q_bounds.sort_unstable();
        q_bounds.dedup();

        let mut prev = *start;
        let mut ordinal = 0u32;
        for b in q_bounds {
            // 退化分段不入列：并进上一题（见 `MIN_QUESTION_MS`）
            if b <= prev + MIN_QUESTION_MS || *end <= b + MIN_QUESTION_MS {
                continue;
            }
            ordinal += 1;
            out.push(Chapter {
                seq: out.len() as u32,
                level: ChapterLevel::Question,
                parent: Some(mat_idx),
                ordinal,
                start_ms: prev,
                end_ms: b,
                title: format!("第 {} 题", ordinal),
                source: ChapterSource::Structure,
                confidence: 0.8,
                locked: false,
            });
            prev = b;
        }
        ordinal += 1;
        if *end > prev {
            out.push(Chapter {
                seq: out.len() as u32,
                level: ChapterLevel::Question,
                parent: Some(mat_idx),
                ordinal,
                start_ms: prev,
                end_ms: *end,
                title: format!("第 {} 题", ordinal),
                source: ChapterSource::Structure,
                confidence: 0.7,
                locked: false,
            });
        }
    }

    // 保证交出去的列表一定是「材料 → 其题」的规范顺序（幂等，见 lime-core）
    normalize_chapters(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digital_silence_is_detected() {
        let sr = 1000;
        let mut x = vec![0.1f32; 1000];
        for v in x.iter_mut().take(600).skip(100) {
            *v = 0.0;
        }
        let spans = digital_silence_spans(&x, sr, 200);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].0 as i64 - 100).abs() <= 1);
        assert!((spans[0].1 as i64 - 600).abs() <= 1);
    }

    #[test]
    fn gaps_classified_by_duration() {
        let p = AnalyzeParams::default();
        assert_eq!(classify(0.2, &p), None);
        assert_eq!(classify(0.4, &p), Some(GapKind::Sentence));
        assert_eq!(classify(2.5, &p), Some(GapKind::Question));
        assert_eq!(classify(9.0, &p), Some(GapKind::Material));
    }

    #[test]
    fn degenerate_question_boundary_is_merged_into_previous() {
        // 材料 1：0–20 s，末尾 12 ms 处有一个"题边界"（数字静音残留）
        let structure = Structure {
            speech: vec![(0, 19_988), (30_000, 40_000)],
            gaps: vec![
                Gap { start_ms: 19_988, end_ms: 20_000, kind: GapKind::Question },
                Gap { start_ms: 20_000, end_ms: 30_000, kind: GapKind::Material },
            ],
            duration_ms: 40_000,
            has_digital_silence: true,
        };
        let chapters = build_chapters(&structure, &[], &[], &AnalyzeParams::default());
        assert!(lime_core::chapters_are_canonical(&chapters), "{chapters:?}");
        let questions: Vec<_> = chapters
            .iter()
            .filter(|c| c.level == ChapterLevel::Question && c.parent == Some(0))
            .collect();
        assert_eq!(questions.len(), 1, "不应出现 0–40 ms 的空题: {chapters:?}");
        assert_eq!(questions[0].start_ms, 0);
        assert_eq!(questions[0].end_ms, 20_000);
    }

    #[test]
    fn boundary_merge_boosts_confidence() {
        let structure = Structure {
            speech: vec![(0, 5000), (15000, 20000)],
            gaps: vec![Gap { start_ms: 5000, end_ms: 15000, kind: GapKind::Material }],
            duration_ms: 20000,
            has_digital_silence: true,
        };
        let chime = Chime {
            ding_ms: 14800,
            dong_ms: 15400,
            ding_db: -16.0,
            dong_db: -23.0,
            confidence: 1.0,
        };
        let b = collect_boundaries(&structure, &[chime], &AnalyzeParams::default());
        assert_eq!(b.len(), 1);
        assert!(b[0].from_gap && b[0].from_chime, "{b:?}");
        assert!(b[0].confidence > 0.95);
        let chapters = build_chapters(&structure, &[], &[chime], &AnalyzeParams::default());
        assert!(lime_core::chapters_are_canonical(&chapters), "{chapters:?}");
        let m: Vec<_> = chapters.iter().filter(|c| c.level == ChapterLevel::Material).collect();
        assert_eq!(m.len(), 2, "{chapters:?}");
        assert_eq!(m[0].start_ms, 0);
        assert_eq!(m[0].end_ms, 5000);
        assert_eq!(m[1].start_ms, 15400);
        // 题必须紧跟自己的材料，且题号从 1 开始逐份材料重排
        let mut mat_no = 0u32;
        for i in chapters
            .iter()
            .enumerate()
            .filter(|(_, c)| c.level == ChapterLevel::Material)
            .map(|(i, _)| i)
        {
            mat_no += 1;
            assert_eq!(chapters[i].ordinal, mat_no);
            let mut n = 0u32;
            for q in chapters[i + 1..]
                .iter()
                .take_while(|c| c.level == ChapterLevel::Question)
            {
                n += 1;
                assert_eq!(q.parent, Some(i));
                assert_eq!(q.ordinal, n);
            }
        }
        assert_eq!(mat_no, 2);
    }
}
