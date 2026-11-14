//! 听力播放进度与试题材料切换的同步逻辑。

use lime_core::Chapter;

/// 根据当前播放位置 `pos_ms` 与章节列表，计算应当展示的材料序号与高亮题号。
///
/// - 若处于材料级章节（含「引言」），返回 `(material_ordinal, None)`；
/// - 若处于子章节「第 N 遍」（同一材料的朗读遍次），保持父材料的 `material_ordinal`，不误判为第 N 题；
/// - 若处于明确标号「第 N 题」的子章节，额外返回高亮题号 `Some(N)`。
pub fn resolve_exercise_sync(
    chapters: &[Chapter],
    pos_ms: u64,
    fallback_mat_ord: u32,
) -> (u32, Option<u32>) {
    let cur_chap = chapters
        .iter()
        .find(|c| pos_ms >= c.start_ms && pos_ms < c.end_ms);

    match cur_chap {
        Some(c) if c.is_material() => {
            let ord = if c.title == lime_core::INTRO_TITLE {
                1
            } else {
                c.ordinal.max(1)
            };
            (ord, None)
        }
        Some(c) => {
            let p_ord = c
                .parent
                .and_then(|p_idx| chapters.get(p_idx))
                .map(|p| p.ordinal.max(1))
                .unwrap_or(1);
            let q_num = c
                .title
                .strip_prefix("第")
                .and_then(|s| s.strip_suffix("题"))
                .and_then(|s| s.trim().parse::<u32>().ok());
            (p_ord, q_num)
        }
        None => (fallback_mat_ord, None),
    }
}

/// 在演示模式下按方向翻页材料序号（1-based，钳位在 `1..=total`）。
pub fn step_material_ordinal(current: u32, total: usize, dir: i32) -> u32 {
    if total == 0 {
        return current;
    }
    let max_ord = total as u32;
    let cur = current.clamp(1, max_ord);
    if dir < 0 {
        cur.saturating_sub(1).max(1)
    } else {
        (cur + 1).min(max_ord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lime_core::{ChapterLevel, ChapterSource};

    fn make_mat(seq: u32, ordinal: u32, start: u64, end: u64, title: &str) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Material,
            parent: None,
            ordinal,
            start_ms: start,
            end_ms: end,
            title: title.into(),
            source: ChapterSource::Structure,
            confidence: 0.9,
            locked: false,
        }
    }

    fn make_pass(seq: u32, ordinal: u32, parent: usize, start: u64, end: u64) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Question,
            parent: Some(parent),
            ordinal,
            start_ms: start,
            end_ms: end,
            title: format!("第 {ordinal} 遍"),
            source: ChapterSource::Structure,
            confidence: 0.8,
            locked: false,
        }
    }

    #[test]
    fn sync_stays_on_parent_material_during_pass_subchapters() {
        let chapters = vec![
            make_mat(0, 0, 0, 10_000, lime_core::INTRO_TITLE),
            make_mat(1, 1, 10_000, 30_000, "材料 1"),
            make_mat(2, 2, 30_000, 70_000, "材料 2"),
            make_pass(3, 1, 2, 30_000, 50_000),
            make_pass(4, 2, 2, 50_000, 70_000),
        ];

        assert_eq!(resolve_exercise_sync(&chapters, 5_000, 1), (1, None));
        assert_eq!(resolve_exercise_sync(&chapters, 15_000, 1), (1, None));
        // 在材料 2 的「第 1 遍」播放时，必须保持在材料 2，而不是误判为第 1 题
        assert_eq!(resolve_exercise_sync(&chapters, 35_000, 1), (2, None));
        assert_eq!(resolve_exercise_sync(&chapters, 55_000, 1), (2, None));
    }

    #[test]
    fn step_material_clamps_to_bounds() {
        assert_eq!(step_material_ordinal(1, 5, -1), 1);
        assert_eq!(step_material_ordinal(1, 5, 1), 2);
        assert_eq!(step_material_ordinal(5, 5, 1), 5);
        assert_eq!(step_material_ordinal(5, 5, -1), 4);
    }
}
