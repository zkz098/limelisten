//! 听力题目导入模式：针对天津高考、全国高考、四六级等典型试卷结构提供精准分组与边界截断。

use crate::{ExerciseParser, ExerciseQuestion};

/// 导入题目时的试卷模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExerciseMode {
    /// 自动识别（通用启发式解析）
    #[default]
    Auto,
    /// 天津高考英语听力（15 题：1~5 短对话单题 + 6~15 长对话/独白，常见 3+3+4）
    TianjinGaokao,
    /// 全国高考英语听力（20 题：1~5 短对话单题 + 6~20 长对话/独白，常见 5 段）
    NationalGaokao,
    /// 大学英语四六级听力（25 题：Section A 1~7, Section B 8~15, Section C 16~25）
    Cet,
}

impl ExerciseMode {
    /// 从 UI 索引转换为题目模式（0=自动，1=天津高考，2=全国高考，3=四六级）。
    pub fn from_index(idx: i32) -> Self {
        match idx {
            1 => Self::TianjinGaokao,
            2 => Self::NationalGaokao,
            3 => Self::Cet,
            _ => Self::Auto,
        }
    }

    /// 该模式下的听力最大题号；超出此题号（如阅读理解第 16/21/26 题）将自动截断。
    pub fn max_question_num(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::TianjinGaokao => Some(15),
            Self::NationalGaokao => Some(20),
            Self::Cet => Some(25),
        }
    }

    /// 判断当前行是否已进入非听力部分（如单项填空、完形填空、阅读理解等）。
    pub fn is_non_listening_section(self, line: &str) -> bool {
        if self == Self::Auto {
            return false;
        }
        let t = line.trim();
        if t.is_empty() || t.chars().count() > 60 {
            return false;
        }
        // 若包含听力关键词，则仍属听力
        if t.contains("听力") || t.to_ascii_lowercase().contains("listening") {
            return false;
        }
        const STOP_KEYWORDS: &[&str] = &[
            "单项填空",
            "单项选择",
            "英语知识运用",
            "语言知识运用",
            "完形填空",
            "阅读理解",
            "阅读表达",
            "书面表达",
            "短文改错",
            "语法填空",
            "选词填空",
            "长篇阅读",
            "仔细阅读",
            "第二部分",
            "第三部分",
            "第四部分",
            "Part II",
            "Part III",
            "Part IV",
            "Reading Comprehension",
            "Cloze",
            "Writing",
            "Translation",
        ];
        let lower = t.to_ascii_lowercase();
        STOP_KEYWORDS
            .iter()
            .any(|kw| t.contains(kw) || lower.contains(&kw.to_ascii_lowercase()))
    }

    /// 根据模式对已解析出的 `(材料标题, 题目列表)` 分组进行优化。
    pub(crate) fn apply_grouping(
        self,
        mut groups: Vec<(String, Vec<ExerciseQuestion>)>,
        known_material_count: usize,
    ) -> Vec<(String, Vec<ExerciseQuestion>)> {
        if groups.is_empty() {
            return groups;
        }

        match self {
            Self::Auto => apply_auto_grouping(groups, known_material_count),
            Self::TianjinGaokao => apply_gaokao_grouping(groups, known_material_count, &[3, 3, 4]),
            Self::NationalGaokao => {
                apply_gaokao_grouping(groups, known_material_count, &[2, 3, 3, 4, 3])
            }
            Self::Cet => {
                if groups.len() == 1 && is_generic_first_title(&groups[0].0) {
                    let (_, questions) = groups.remove(0);
                    return group_cet_questions(questions, known_material_count);
                }
                groups
            }
        }
    }
}

fn is_generic_first_title(title: &str) -> bool {
    title == "材料 1" || title.contains("第一节")
}

fn apply_auto_grouping(
    mut groups: Vec<(String, Vec<ExerciseQuestion>)>,
    known_material_count: usize,
) -> Vec<(String, Vec<ExerciseQuestion>)> {
    if groups.len() == 1 && groups[0].0 == "材料 1" && known_material_count > 1 {
        let (_, questions) = groups.remove(0);
        return ExerciseParser::split_questions_evenly(questions, known_material_count);
    }

    if groups.len() > 1 && known_material_count > groups.len() {
        let first_q_count = groups[0].1.len();
        if first_q_count > 1 && (first_q_count + groups.len() - 1) == known_material_count {
            return expand_first_group(groups);
        }
    }

    groups
}

/// 高考（天津/全国）通用分组：
/// - 第一节 1~5 题：每题独立为一段短对话材料；
/// - 第二节 6~15 / 6~20 题：若无引导语，按典型长对话/独白题数分布（或剩余材料数）分组。
fn apply_gaokao_grouping(
    mut groups: Vec<(String, Vec<ExerciseQuestion>)>,
    known_material_count: usize,
    default_sec2_chunks: &[usize],
) -> Vec<(String, Vec<ExerciseQuestion>)> {
    // 情况 1：完全没有材料头（仅 1 组），按 1~5 单题 + 第二节典型分组拆分
    if groups.len() == 1 && is_generic_first_title(&groups[0].0) {
        let (_, questions) = groups.remove(0);
        return split_gaokao_flat(questions, known_material_count, default_sec2_chunks);
    }

    // 情况 2：有第二节材料头，但第一节（1~5 题）被合在一组里 -> 展开为 5 个单题短对话
    if groups.len() > 1 {
        let first_len = groups[0].1.len();
        let looks_like_sec1 = first_len == 5
            && groups[0].1.first().is_some_and(|q| q.num == 1)
            && groups[0].1.last().is_some_and(|q| q.num == 5);
        if looks_like_sec1
            && (known_material_count > groups.len() || groups[0].0.contains("第一节"))
        {
            return expand_first_group(groups);
        }
    }

    groups
}

fn expand_first_group(
    mut groups: Vec<(String, Vec<ExerciseQuestion>)>,
) -> Vec<(String, Vec<ExerciseQuestion>)> {
    let (first_title, first_questions) = groups.remove(0);
    let mut expanded = Vec::with_capacity(first_questions.len() + groups.len());
    for q in first_questions {
        let title = format!("{first_title} - 第{}题", q.num);
        expanded.push((title, vec![q]));
    }
    expanded.extend(groups);
    expanded
}

fn split_gaokao_flat(
    questions: Vec<ExerciseQuestion>,
    known_material_count: usize,
    default_sec2_chunks: &[usize],
) -> Vec<(String, Vec<ExerciseQuestion>)> {
    if questions.len() <= 5 {
        return questions
            .into_iter()
            .map(|q| (format!("短对话 第{}题", q.num), vec![q]))
            .collect();
    }

    let mut sec1 = Vec::new();
    let mut sec2 = Vec::new();
    for q in questions {
        if q.num <= 5 && sec1.len() < 5 {
            sec1.push(q);
        } else {
            sec2.push(q);
        }
    }

    let mut out: Vec<(String, Vec<ExerciseQuestion>)> = sec1
        .into_iter()
        .map(|q| (format!("第一节 - 第{}题", q.num), vec![q]))
        .collect();

    if sec2.is_empty() {
        return out;
    }

    let expected_sec2_total: usize = default_sec2_chunks.iter().sum();
    if sec2.len() == expected_sec2_total
        && (known_material_count <= 1
            || known_material_count == out.len() + default_sec2_chunks.len())
    {
        let mut iter = sec2.into_iter();
        for (idx, &size) in default_sec2_chunks.iter().enumerate() {
            let chunk: Vec<_> = iter.by_ref().take(size).collect();
            if !chunk.is_empty() {
                let first_n = chunk.first().map(|q| q.num).unwrap_or(0);
                let last_n = chunk.last().map(|q| q.num).unwrap_or(0);
                out.push((
                    format!("第二节 材料{}（第{first_n}-{last_n}题）", idx + 1),
                    chunk,
                ));
            }
        }
        return out;
    }

    let rem_mats = known_material_count
        .saturating_sub(out.len())
        .max(default_sec2_chunks.len().min(sec2.len()));
    let mut sec2_groups = ExerciseParser::split_questions_evenly(sec2, rem_mats);
    for (idx, (title, _)) in sec2_groups.iter_mut().enumerate() {
        *title = format!("第二节 材料{}", idx + 1);
    }
    out.extend(sec2_groups);
    out
}

fn group_cet_questions(
    questions: Vec<ExerciseQuestion>,
    known_material_count: usize,
) -> Vec<(String, Vec<ExerciseQuestion>)> {
    if questions.len() == 25 && known_material_count <= 1 {
        // 标准四六级分布: News Reports (2+2+3), Conversations (4+4), Passages (3+3+4)
        let chunks = [
            ("Section A News 1", 2usize),
            ("Section A News 2", 2),
            ("Section A News 3", 3),
            ("Section B Conversation 1", 4),
            ("Section B Conversation 2", 4),
            ("Section C Passage 1", 3),
            ("Section C Passage 2", 3),
            ("Section C Passage 3", 4),
        ];
        let mut iter = questions.into_iter();
        let mut out = Vec::with_capacity(chunks.len());
        for (title, size) in chunks {
            let chunk: Vec<_> = iter.by_ref().take(size).collect();
            if !chunk.is_empty() {
                out.push((title.to_string(), chunk));
            }
        }
        return out;
    }
    ExerciseParser::split_questions_evenly(questions, known_material_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tianjin_mode_truncates_after_15_and_expands_sec1() {
        let mut text = String::from(
            "第一部分 听力\n第一节（共5小题；每小题1分，满分5分）\n\
             听下面五段对话。每段对话后有一个小题。\n",
        );
        for i in 1..=5 {
            text.push_str(&format!("{i}. Q{i}?\nA. A{i}\nB. B{i}\nC. C{i}\n"));
        }
        text.push_str("第二节（共10小题；每小题1.5分，满分15分）\n");
        text.push_str("听下面一段对话，回答第6至第8小题。\n");
        for i in 6..=8 {
            text.push_str(&format!("{i}. Q{i}?\nA. A{i}\nB. B{i}\nC. C{i}\n"));
        }
        text.push_str("听下面一段材料，回答第9至第11小题。\n");
        for i in 9..=11 {
            text.push_str(&format!("{i}. Q{i}?\nA. A{i}\nB. B{i}\nC. C{i}\n"));
        }
        text.push_str("听下面一段独白，回答第12至第15小题。\n");
        for i in 12..=15 {
            text.push_str(&format!("{i}. Q{i}?\nA. A{i}\nB. B{i}\nC. C{i}\n"));
        }
        // 混入后续笔试题（第 1 题重置，或第 16 题）
        text.push_str("16. Reading Q16?\nA. X\nB. Y\nC. Z\nD. W\n");

        let parsed = ExerciseParser::parse_with_mode(&text, 8, ExerciseMode::TianjinGaokao);
        let total: usize = parsed.iter().map(|m| m.questions.len()).sum();
        assert_eq!(total, 15, "天津高考模式应只保留 1~15 题");
        assert_eq!(parsed.len(), 8, "应分为 5 个短对话 + 3 个长材料");
        assert_eq!(parsed[0].questions.len(), 1);
        assert_eq!(parsed[5].questions.len(), 3);
        assert_eq!(parsed[6].questions.len(), 3);
        assert_eq!(parsed[7].questions.len(), 4);
    }

    #[test]
    fn tianjin_mode_flat_questions_auto_grouped() {
        let mut text = String::new();
        for i in 1..=15 {
            text.push_str(&format!("{i}. Question {i}?\nA. One\nB. Two\nC. Three\n"));
        }
        let parsed = ExerciseParser::parse_with_mode(&text, 1, ExerciseMode::TianjinGaokao);
        assert_eq!(parsed.len(), 8);
        assert_eq!(parsed[0].questions[0].num, 1);
        assert_eq!(parsed[5].questions.len(), 3);
        assert_eq!(parsed[6].questions.len(), 3);
        assert_eq!(parsed[7].questions.len(), 4);
    }

    #[test]
    fn national_mode_truncates_at_21_and_groups_20_questions() {
        let mut text = String::new();
        for i in 1..=25 {
            text.push_str(&format!("{i}. Question {i}?\nA. Opt1\nB. Opt2\nC. Opt3\n"));
        }
        let parsed = ExerciseParser::parse_with_mode(&text, 10, ExerciseMode::NationalGaokao);
        let total: usize = parsed.iter().map(|m| m.questions.len()).sum();
        assert_eq!(total, 20, "全国高考模式应在第 21 题截断");
        assert_eq!(parsed.len(), 10, "应分为 5 短对话 + 5 长材料");
    }

    #[test]
    fn tianjin_mode_stops_on_question_number_reset_for_multiple_choice() {
        let mut text = String::new();
        for i in 1..=15 {
            text.push_str(&format!("{i}. Listening {i}?\nA. A\nB. B\nC. C\n"));
        }
        // 天津高考笔试第一部分单项填空也从 1 开始
        text.push_str("1. Grammar question?\nA. A\nB. B\nC. C\nD. D\n");
        let parsed = ExerciseParser::parse_with_mode(&text, 8, ExerciseMode::TianjinGaokao);
        let total: usize = parsed.iter().map(|m| m.questions.len()).sum();
        assert_eq!(total, 15, "题号重置为 1 时应停止解析");
    }
}
