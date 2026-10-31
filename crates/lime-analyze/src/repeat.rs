//! 相邻材料的「同一材料被朗读多遍」检测与合并（切分的语义兜底）。
//!
//! 背景（见 `docs/GRILLING.md` §2.1 / chime.rs 模块头 §5）：
//! 本类素材里同一材料常被朗读两遍。切分骨架（静音 + 叮咚）有时会在「读第二遍」的分界处
//! 给出一个材料级切点（真叮咚、或两遍之间间隔 ≥ `gap_material_s`），于是同一份材料裂成
//! 两份。DSP 层已尽量收紧（chime.rs 的音调纯度闸门），但语义上仍需要一个与声学无关的
//! 兜底：**如果后一份材料的文本几乎是前一份的重复，就认定它是同一材料的第 N 遍。**
//!
//! 判据：取每份材料时间范围内的英文词元（小写、只留 ASCII 字母数字），做词级 LCS，
//! 用 Sørensen–Dice 系数 `2·LCS/(|a|+|b|)` 作为相似度。实测 4 个素材：
//! - 真正的两遍：0.97 / 1.00；
//! - 所有相邻的不同材料：≤ 0.20。
//!
//! 阈值 0.65、最少 8 词，有巨大余量。中文念白（无 ASCII 词元）天然被跳过。
//!
//! 合并结果：一份 `Material` 覆盖两遍，两个「第 1 遍 / 第 2 遍」`Question` 子章节
//! （人工锁定的材料不参与合并）。

use lime_core::{normalize_chapters, Chapter, ChapterLevel, Sentence};
use std::collections::HashMap;

/// 重复检测参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepeatParams {
    /// 参与比较的最少词数（低于此值不判重复，避免把短念白/公告并进来）。
    pub min_tokens: usize,
    /// 相似度阈值。
    pub min_similarity: f32,
    /// 两遍时长（词数）比例上限。
    pub max_len_ratio: f32,
}

impl Default for RepeatParams {
    fn default() -> Self {
        Self {
            min_tokens: 8,
            min_similarity: 0.65,
            max_len_ratio: 2.5,
        }
    }
}

/// 合并结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatMerge {
    /// 规范化后的章节列表。
    pub chapters: Vec<Chapter>,
    /// 被并掉的重复材料数（0 = 没有任何重复）。
    pub merged: usize,
}

/// 合并相邻的重复材料（默认参数）。返回规范化的章节列表。
pub fn merge_repeated_materials(chapters: &[Chapter], sentences: &[Sentence]) -> Vec<Chapter> {
    merge_repeated_materials_with(chapters, sentences, &RepeatParams::default()).chapters
}

/// 带参数与合并计数的版本。
pub fn merge_repeated_materials_with(
    chapters: &[Chapter],
    sentences: &[Sentence],
    params: &RepeatParams,
) -> RepeatMerge {
    let list = normalize_chapters(chapters);
    let n = list.len();
    if n == 0 {
        return RepeatMerge {
            chapters: list,
            merged: 0,
        };
    }

    // 题的父材料下标（normalize 之后已重建，可靠）
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut loose: Vec<usize> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        if item.is_material() {
            continue;
        }
        match item.parent {
            Some(m) if m < n => children[m].push(i),
            _ => loose.push(i),
        }
    }

    let mats: Vec<usize> = (0..n).filter(|&i| list[i].is_material()).collect();
    let mut consumed = vec![false; n];
    let mut out: Vec<Chapter> = Vec::with_capacity(n);
    let mut merged = 0usize;

    // 预先算好每份材料的词元，避免重复扫描句子
    let tokens: Vec<Vec<String>> = (0..n)
        .map(|i| material_tokens(&list[i], sentences))
        .collect();

    for (pos, &mi) in mats.iter().enumerate() {
        if consumed[mi] {
            continue;
        }
        // 往后找连续重复的材料（跳过已消费的与题）
        let mut run = vec![mi];
        let mut p = pos + 1;
        while p < mats.len() {
            let mj = mats[p];
            p += 1;
            if consumed[mj] {
                continue;
            }
            if is_repeat(&tokens[mi], &tokens[mj], params) {
                run.push(mj);
                consumed[mj] = true;
            } else {
                break;
            }
        }

        // 人工锁定时不合并（尊重用户修正）
        let locked = run
            .iter()
            .any(|&m| list[m].locked || children[m].iter().any(|&q| list[q].locked));
        if run.len() == 1 || locked {
            for &m in &run {
                out.push(list[m].clone());
                for &q in &children[m] {
                    out.push(list[q].clone());
                }
            }
            continue;
        }

        merged += run.len() - 1;
        let mut mat = list[mi].clone();
        mat.start_ms = list[mi].start_ms;
        mat.end_ms = run
            .iter()
            .map(|&m| list[m].end_ms)
            .max()
            .unwrap_or(list[mi].end_ms);
        mat.level = ChapterLevel::Material;
        mat.parent = None;
        mat.locked = false;
        out.push(mat);
        // 每一遍降级为一个题级子章节，标题「第 N 遍」（normalize 不会改写它）
        for (r, &m) in run.iter().enumerate() {
            let mut q = list[m].clone();
            q.level = ChapterLevel::Question;
            q.parent = None;
            q.ordinal = r as u32 + 1;
            q.title = format!("第 {} 遍", r + 1);
            q.locked = false;
            out.push(q);
        }
    }

    for &i in &loose {
        out.push(list[i].clone());
    }

    RepeatMerge {
        chapters: normalize_chapters(&out),
        merged,
    }
}

/// 一份材料时间范围内的英文词元（小写、只留 ASCII 字母数字）。
fn material_tokens(c: &Chapter, sentences: &[Sentence]) -> Vec<String> {
    let mut out = Vec::new();
    for s in sentences {
        let mid = s.start_ms + s.end_ms.saturating_sub(s.start_ms) / 2;
        if mid >= c.start_ms && mid < c.end_ms {
            out.extend(
                s.text
                    .split(|ch: char| !ch.is_ascii_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(|w| w.to_ascii_lowercase()),
            );
        }
    }
    out
}

fn is_repeat(a: &[String], b: &[String], p: &RepeatParams) -> bool {
    if a.len() < p.min_tokens || b.len() < p.min_tokens {
        return false;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if long.len() as f32 > short.len() as f32 * p.max_len_ratio {
        return false;
    }
    if a == b {
        return true;
    }
    // O(n+m) 上界预筛：LCS 不可能超过两份词表的多重集交集。
    // 相邻的不同材料通常在这里就被排除，不再进 O(n·m) 的 LCS；
    // 只有疑似重复（真正的第二遍朗读）才支付完整代价。
    let common = multiset_overlap_upper_bound(a, b) as f32;
    if 2.0 * common / ((a.len() + b.len()) as f32) < p.min_similarity {
        return false;
    }
    let l = lcs_len(a, b) as f32;
    let sim = 2.0 * l / (a.len() + b.len()) as f32;
    sim >= p.min_similarity
}

/// 两份词表的多重集交集大小（同时是 LCS 的严格上界）。
fn multiset_overlap_upper_bound(a: &[String], b: &[String]) -> usize {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(long.len());
    for w in long {
        *counts.entry(w.as_str()).or_insert(0) += 1;
    }
    let mut common = 0;
    for w in short {
        if let Some(c) = counts.get_mut(w.as_str()) {
            if *c > 0 {
                *c -= 1;
                common += 1;
            }
        }
    }
    common
}

/// 词级最长公共子序列长度（滚动数组，O(n·m)）。
fn lcs_len(a: &[String], b: &[String]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lime_core::ChapterSource;

    fn mat(seq: u32, start: u64, end: u64) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Material,
            parent: None,
            ordinal: seq + 1,
            start_ms: start,
            end_ms: end,
            title: format!("材料 {}", seq + 1),
            source: ChapterSource::Structure,
            confidence: 0.9,
            locked: false,
        }
    }

    /// 在 [start,end) 内按固定步长铺一句话，词来自 `text`。
    fn sent(start: u64, end: u64, text: &str) -> Sentence {
        Sentence {
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
            words: Vec::new(),
            chapter: None,
        }
    }

    fn reading_a(t0: u64) -> Sentence {
        sent(
            t0,
            t0 + 10_000,
            "John have you seen Jeff not since he left my office an hour ago why he is supposed to be",
        )
    }

    fn reading_b(t0: u64) -> Sentence {
        // 与 A 高度重叠（模拟第二遍；ASR 可能少一两个词）
        sent(
            t0,
            t0 + 10_000,
            "not since he left my office an hour ago why he is supposed to be at the airport now",
        )
    }

    #[test]
    fn merges_same_material_read_twice() {
        let chapters = vec![mat(0, 0, 12_000), mat(1, 13_000, 25_000)];
        let sentences = vec![reading_a(0), reading_b(13_000)];
        let rep = merge_repeated_materials_with(&chapters, &sentences, &RepeatParams::default());
        assert_eq!(rep.merged, 1);
        let mats: Vec<_> = rep.chapters.iter().filter(|c| c.is_material()).collect();
        assert_eq!(mats.len(), 1, "{:?}", rep.chapters);
        assert_eq!(mats[0].start_ms, 0);
        assert_eq!(mats[0].end_ms, 25_000);
        let titles: Vec<_> = rep.chapters.iter().map(|c| c.title.clone()).collect();
        assert_eq!(titles, vec!["引言", "第 1 遍", "第 2 遍"], "{titles:?}");
        assert!(lime_core::chapters_are_canonical(&rep.chapters));
        // 幂等
        let again = merge_repeated_materials(&rep.chapters, &sentences);
        assert_eq!(again, rep.chapters);
    }

    #[test]
    fn keeps_distinct_materials() {
        let chapters = vec![mat(0, 0, 12_000), mat(1, 13_000, 25_000)];
        let sentences = vec![
            reading_a(0),
            sent(
                13_000,
                23_000,
                "the quick brown fox jumps over the lazy dog every single morning",
            ),
        ];
        let rep = merge_repeated_materials_with(&chapters, &sentences, &RepeatParams::default());
        assert_eq!(rep.merged, 0);
        assert_eq!(rep.chapters.iter().filter(|c| c.is_material()).count(), 2);
    }

    /// 多重集交集是 LCS 的上界；不同材料的交集应当远低于阈值，从而跳过 O(n·m) 的 LCS。
    #[test]
    fn multiset_overlap_is_a_safe_lcs_upper_bound() {
        let a: Vec<String> = "the quick brown fox jumps over the lazy dog today"
            .split(' ')
            .map(String::from)
            .collect();
        let b: Vec<String> = "a completely different passage about listening practice"
            .split(' ')
            .map(String::from)
            .collect();
        let overlap = multiset_overlap_upper_bound(&a, &b);
        assert!(overlap >= lcs_len(&a, &b));
        let dice_upper = 2.0 * overlap as f32 / (a.len() + b.len()) as f32;
        assert!(
            dice_upper < RepeatParams::default().min_similarity,
            "不同材料不应进入 LCS: {dice_upper}"
        );
    }

    #[test]
    fn ignores_short_announcements() {
        // 中文念白没有 ASCII 词元；即便两份短材料词表相同也不合并
        let chapters = vec![mat(0, 0, 12_000), mat(1, 13_000, 25_000)];
        let sentences = vec![
            sent(0, 4_000, "听下面一段对话 回答第九至第十一小题"),
            sent(13_000, 17_000, "听下面一段对话 回答第九至第十一小题"),
        ];
        let rep = merge_repeated_materials_with(&chapters, &sentences, &RepeatParams::default());
        assert_eq!(rep.merged, 0);
    }

    #[test]
    fn locked_material_is_not_merged() {
        let mut a = mat(0, 0, 12_000);
        a.locked = true;
        let chapters = vec![a, mat(1, 13_000, 25_000)];
        let sentences = vec![reading_a(0), reading_b(13_000)];
        let rep = merge_repeated_materials_with(&chapters, &sentences, &RepeatParams::default());
        assert_eq!(rep.merged, 0);
        assert_eq!(rep.chapters.iter().filter(|c| c.is_material()).count(), 2);
    }
}
