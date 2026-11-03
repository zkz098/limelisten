//! 套卷分区：从完整试卷文本中切出听力部分。
//!
//! 只识别顶层章节标题（如「第一部分 听力」「Part I Listening Comprehension」），
//! 不处理材料级标题（如 `Text 1`、`Section A`），后者仍交给 [`crate::ExerciseParser`]。

use crate::exercise_parser::sanitize_input;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningSection {
    /// 是否识别到明确的听力分区标题。
    pub found: bool,
    /// 切出的听力文本；未识别到分区时为清理后的全文。
    pub text: String,
}

/// 从套卷文本中提取听力部分。未识别到顶层分区标题时回退为全文。
pub fn extract_listening_section(input: &str) -> ListeningSection {
    let sanitized = sanitize_input(input);
    let lines: Vec<&str> = sanitized.lines().collect();

    let mut start = None;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let Some(kind) = classify_section_heading(line, &lines[index + 1..]) else {
            continue;
        };
        match kind {
            SectionKind::Listening => {
                if start.is_none() {
                    start = Some(index);
                }
            }
            SectionKind::Other => {
                if start.is_some() {
                    end = index;
                    break;
                }
            }
        }
    }

    match start {
        Some(start) => ListeningSection {
            found: true,
            text: lines[start..end].join("\n").trim().to_string(),
        },
        None => ListeningSection {
            found: false,
            text: sanitized.trim().to_string(),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Listening,
    Other,
}

fn classify_section_heading(line: &str, following: &[&str]) -> Option<SectionKind> {
    let line = line.trim();
    if line.is_empty() || line.chars().count() > 80 {
        return None;
    }

    let is_cn_part = Regex::new(r"^第[一二三四五六七八九十百0-9]+部分")
        .unwrap()
        .is_match(line);
    let is_en_part = Regex::new(r"(?i)^part\s+(?:[ivxlc]+|\d+)\b")
        .unwrap()
        .is_match(line);
    let is_cn_named = Regex::new(r"^第[一二三四五六七八九十百0-9]+节\s*[:：]?\s*\S.*$")
        .unwrap()
        .is_match(line);
    let is_en_named = Regex::new(r"(?i)^section\s+(?:[a-z]|[ivxlc]+|\d+)\b\s*[:：]?\s*\S.*$")
        .unwrap()
        .is_match(line);
    let is_cn_keyword = Regex::new(
        r"^(?:听力|听力理解|听力部分|听力测试|阅读|阅读理解|阅读部分|完形填空|写作|翻译|语法|词汇|英语知识运用)\s*(?:[（(].*)?$",
    )
    .unwrap()
    .is_match(line);
    let is_en_keyword = Regex::new(
        r"(?i)^(?:listening(?:\s+(?:comprehension|test|section))?|reading(?:\s+(?:comprehension|test|section))?|writing|translation)\s*(?:[（(].*)?$",
    )
    .unwrap()
    .is_match(line);
    let is_cn_numbered = Regex::new(r"^[一二三四五六七八九十]+[、.．:：\s]\s*\S.*$")
        .unwrap()
        .is_match(line);
    let is_roman_numbered = Regex::new(r"^[IVXLC]+[、.．:：\)]\s*\S.*$")
        .unwrap()
        .is_match(line);

    if !(is_cn_part
        || is_en_part
        || is_cn_named
        || is_en_named
        || is_cn_keyword
        || is_en_keyword
        || is_cn_numbered
        || is_roman_numbered)
    {
        return None;
    }

    let lower = line.to_lowercase();
    let mentions_listening = lower.contains("listening") || line.contains("听力");
    let mentions_other = lower.contains("reading")
        || lower.contains("writing")
        || lower.contains("translation")
        || line.contains("阅读")
        || line.contains("完形")
        || line.contains("写作")
        || line.contains("翻译")
        || line.contains("语法")
        || line.contains("词汇")
        || line.contains("英语知识运用");

    if mentions_listening {
        return Some(SectionKind::Listening);
    }
    if mentions_other {
        return Some(SectionKind::Other);
    }

    // 裸「第一部分 / Part I」：用后续几行判断是否进入听力，
    // 无法判断时按非听力分区处理。
    if is_cn_part || is_en_part {
        let looks_listening = following
            .iter()
            .take(6)
            .any(|next| looks_like_listening_start(next));
        return Some(if looks_listening {
            SectionKind::Listening
        } else {
            SectionKind::Other
        });
    }

    None
}

fn looks_like_listening_start(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    lower.contains("listening")
        || lower.contains("you will hear")
        || lower.contains("conversation")
        || lower.starts_with("text ")
        || line.contains("听力")
        || line.contains("听下面")
        || line.contains("听两遍")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExerciseParser;

    #[test]
    fn extracts_chinese_listening_section_from_full_paper() {
        let paper = r#"
第一部分 听力（共两节，满分30分）
第一节
听下面5段对话。每段对话后有一个小题。
Text 1
1. What is the woman doing?
A. Driving a car.
B. Walking in the park.
答案: A
Text 2
2. Where are the speakers?
A. In a library.
B. In a bank.
答案: B
第二部分 阅读理解（共两节，满分40分）
第一节
阅读下列短文，从每题所给的A、B、C、D四个选项中选出最佳选项。
Passage 1
21. What is the main idea?
A. Reading.
B. Writing.
"#;

        let section = extract_listening_section(paper);
        assert!(section.found);
        assert!(!section.text.contains("阅读理解"));
        assert!(!section.text.contains("21."));

        let parsed = ExerciseParser::parse(&section.text, 2);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].title, "Text 1");
        assert_eq!(parsed[1].title, "Text 2");
        assert_eq!(parsed[0].questions[0].num, 1);
        assert_eq!(parsed[1].questions[0].num, 2);
    }

    #[test]
    fn extracts_english_listening_section() {
        let paper = r#"
Part I Listening Comprehension (30 minutes)
Section A
Conversation 1
1. What does the man want?
A. A book.
B. A pen.
Part II Reading Comprehension
Passage 1
21. What is the passage about?
A. Music.
B. Sports.
"#;

        let section = extract_listening_section(paper);
        assert!(section.found);
        assert!(section.text.contains("Conversation 1"));
        assert!(!section.text.contains("Reading Comprehension"));
        assert!(!section.text.contains("21."));
    }

    #[test]
    fn handles_keyword_only_and_bare_part_headings() {
        let keyword_paper = r#"
Listening Comprehension
Conversation 1
1. What does the man want?
Reading Comprehension
Passage 1
21. What is the passage about?
"#;
        let section = extract_listening_section(keyword_paper);
        assert!(section.found);
        assert!(!section.text.contains("21."));

        let bare_parts = r#"
Part I
Directions: In this section, you will hear five conversations.
Conversation 1
1. What does the man want?
Part II
Directions: Read the following passage.
Passage 1
21. What is the passage about?
"#;
        let section = extract_listening_section(bare_parts);
        assert!(section.found);
        assert!(section.text.contains("Conversation 1"));
        assert!(!section.text.contains("21."));
    }

    #[test]
    fn falls_back_to_full_text_without_section_heading() {
        let text = "Text 1\n1. What is the woman doing?\nA. Driving.\nB. Walking.";
        let section = extract_listening_section(text);
        assert!(!section.found);
        assert_eq!(section.text, text);
    }

    #[test]
    fn section_heading_requires_title_or_keyword() {
        assert_eq!(classify_section_heading("听力原文", &[]), None);
        assert_eq!(classify_section_heading("第一节", &[]), None);
        assert_eq!(
            classify_section_heading("第一节 听力", &[]),
            Some(SectionKind::Listening)
        );
        assert_eq!(
            classify_section_heading("第一部分：听力", &[]),
            Some(SectionKind::Listening)
        );
    }

    #[test]
    fn extraction_is_idempotent() {
        let paper = r#"
第一部分 听力
Text 1
1. What is the woman doing?
A. Driving.
B. Walking.
第二部分 阅读
Passage 1
21. What is the main idea?
"#;
        let once = extract_listening_section(paper);
        let twice = extract_listening_section(&once.text);
        assert!(twice.found);
        assert_eq!(twice.text, once.text);
    }
}
