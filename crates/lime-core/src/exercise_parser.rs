//! 智能听力题目解析器：支持纯文本/Markdown 启发式解析与 JSON 结构化解析。

use crate::{ExerciseMode, ExerciseQuestion, MaterialExercise};
use regex::Regex;
use std::collections::HashMap;

pub struct ExerciseParser;

impl ExerciseParser {
    /// 解析输入的文本或 JSON 字符串（自动模式）。
    /// `known_material_count` 是音频当前切分出的材料数（去除引言后），用于在未检测到显式材料头时智能分组。
    pub fn parse(input: &str, known_material_count: usize) -> Vec<MaterialExercise> {
        Self::parse_with_mode(input, known_material_count, ExerciseMode::Auto)
    }

    /// 按指定题目模式解析输入的文本或 JSON 字符串。
    pub fn parse_with_mode(
        input: &str,
        known_material_count: usize,
        mode: ExerciseMode,
    ) -> Vec<MaterialExercise> {
        let sanitized = sanitize_input(input);
        let trimmed = sanitized.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        // 1. 尝试作为 JSON 解析
        if let Ok(materials) = serde_json::from_str::<Vec<MaterialExercise>>(trimmed) {
            if !materials.is_empty() {
                return materials;
            }
        }
        if let Ok(questions) = serde_json::from_str::<Vec<ExerciseQuestion>>(trimmed) {
            if !questions.is_empty() {
                let groups = mode.apply_grouping(
                    vec![("材料 1".to_string(), questions)],
                    known_material_count,
                );
                return Self::build_material_exercises(groups);
            }
        }

        // 2. 启发式文本/Markdown 解析
        Self::parse_text(trimmed, known_material_count, mode)
    }

    /// 启发式纯文本解析
    fn parse_text(
        text: &str,
        known_material_count: usize,
        mode: ExerciseMode,
    ) -> Vec<MaterialExercise> {
        let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();

        // 正则表达式定义
        // 材料标题行，支持常见的英文与中文各类标题形式：
        // 1. 标准前缀: "Text 1", "Passage 2", "Section A", "Conversation 1", "Dialogue 1", "材料 1", "听力材料"
        // 2. 中文节/部分: "第一节", "第二节", "第一部分" 等
        // 3. 英文 Part: "Part I", "Part A", "Part 1" 等
        // 4. 听力引导语: "听下面一段对话，回答第6至第8小题。", "听下面一段独白...", "听下面5段对话..."
        // 5. 英文引导语: "Questions 6 to 8 are based on...", "Listen to..."
        // 6. 顶层听力总标题: "一. 听力练习", "一、听力理解"
        let re_mat = Regex::new(concat!(
            r"(?i)^(?:Text|Passage|Section|Conversation|Dialogue|材料|听力材料)\s*([0-9A-Za-z一二三四五六七八九十]+)?.*$|",
            r"^第\s*[一二三四五六七八九十0-9]+\s*[节部分].*$|",
            r"(?i)^Part\s+[A-Za-z0-9IVXLC]+.*$|",
            r"^听(?:下面|第)\s*[0-9一二三四五六七八九十两\s]*段?\s*(?:对话|独白|录音|材料|话).*$|",
            r"(?i)^Questions?\s+\d+\s*(?:to|-|—)\s*\d+.*$|",
            r"^[一二三四五六七八九十]+[、.．:：\s]\s*听力.*$"
        )).unwrap();

        // 题干行，例如: "1. What is the woman doing?", "第 2 题: ...", "3、..."
        let re_stem = Regex::new(r"^(?:第\s*)?(\d+)[\.、\s\)\:\-]+\s*(.+)$").unwrap();

        // 答案行，例如: "答案: A", "Key: B", "[答案] C", "Answer: A"
        let re_answer =
            Regex::new(r"(?i)^(?:【?答案】?|【?Keys?】?|【?Answers?】?)[:：\s]*([A-Ga-g])\b")
                .unwrap();

        // 解析行，例如: "解析: 女士提到了...", "Explanation: ..."
        let re_expl =
            Regex::new(r"(?i)^(?:【?解析】?|【?Explanation】?|【?Notes?】?)[:：\s]*(.+)$").unwrap();

        // 集中答案块特征行，例如: "【参考答案】", "Keys:"
        let re_end_answer_block =
            Regex::new(r"(?i)^[【\[]?(?:参考答案|答案|Keys?|Answers?)[】\]]?$").unwrap();

        let end_answers = Self::collect_end_answers(&lines, &re_end_answer_block);

        let mut groups: Vec<(String, Vec<ExerciseQuestion>)> = Vec::new();
        let mut cur_mat_title = String::new();
        let mut cur_questions: Vec<ExerciseQuestion> = Vec::new();
        let mut last_seen_q_num: Option<u32> = None;

        let mut cur_q_num: Option<u32> = None;
        let mut cur_q_stem = String::new();
        let mut cur_q_opts: Vec<String> = Vec::new();
        let mut cur_q_ans: Option<String> = None;
        let mut cur_q_expl: Option<String> = None;

        let flush_current_question =
            |cur_num: &mut Option<u32>,
             cur_stem: &mut String,
             cur_opts: &mut Vec<String>,
             cur_ans: &mut Option<String>,
             cur_expl: &mut Option<String>,
             questions: &mut Vec<ExerciseQuestion>,
             end_ans_map: &HashMap<u32, String>| {
                if let Some(num) = *cur_num {
                    if !cur_stem.is_empty() {
                        let mut q =
                            ExerciseQuestion::new(num, cur_stem.trim(), std::mem::take(cur_opts));
                        let final_ans = cur_ans.take().or_else(|| end_ans_map.get(&num).cloned());
                        if let Some(a) = final_ans {
                            q.answer = Some(a);
                        }
                        if let Some(e) = cur_expl.take() {
                            q.explanation = Some(e.trim().to_string());
                        }
                        questions.push(q);
                    }
                }
                *cur_num = None;
                cur_stem.clear();
            };

        for line in &lines {
            if line.is_empty() {
                continue;
            }

            // 遇到文末答案块标记，停止提取正文
            if re_end_answer_block.is_match(line) {
                break;
            }

            // 模式截断：已解析出听力题后遇到非听力大题（如单项填空、完形填空、阅读理解）
            if last_seen_q_num.is_some() && mode.is_non_listening_section(line) {
                break;
            }

            // 1. 判断是否是材料分割行
            // 条件：匹配 re_mat 且不包含题号特征，且长度不过长（<= 80 Unicode 字符，避免中文 UTF-8 字节溢出）
            if line.chars().count() <= 80 && re_mat.is_match(line) && !re_stem.is_match(line) {
                // 若当前已有一级节标题（如"第一节..."/"第二节..."）且尚未收集到题目，
                // 紧随其后的总说明（如"听下面五段对话。"/"听下面几段材料。"）不覆盖节标题。
                if cur_q_num.is_none()
                    && cur_questions.is_empty()
                    && !cur_mat_title.is_empty()
                    && Self::is_section_instruction(line)
                {
                    continue;
                }

                flush_current_question(
                    &mut cur_q_num,
                    &mut cur_q_stem,
                    &mut cur_q_opts,
                    &mut cur_q_ans,
                    &mut cur_q_expl,
                    &mut cur_questions,
                    &end_answers,
                );

                if !cur_questions.is_empty() {
                    let title = if cur_mat_title.is_empty() {
                        format!("材料 {}", groups.len() + 1)
                    } else {
                        cur_mat_title.clone()
                    };
                    groups.push((title, std::mem::take(&mut cur_questions)));
                }
                cur_mat_title = line
                    .trim_end_matches(['。', '！', '!', '；', ';', '.'])
                    .to_string();
                continue;
            }

            // 2. 答案行
            if let Some(cap) = re_answer.captures(line) {
                if let Some(m) = cap.get(1) {
                    cur_q_ans = Some(m.as_str().to_ascii_uppercase());
                }
                continue;
            }

            // 3. 解析行
            if let Some(cap) = re_expl.captures(line) {
                if let Some(m) = cap.get(1) {
                    cur_q_expl = Some(m.as_str().to_string());
                }
                continue;
            }

            // 4. 选项行探测（支持同行多个选项 "A. ... B. ..." 或单行 "A. ..."）
            if cur_q_num.is_some() {
                let opts = Self::extract_options_from_line(line);
                if !opts.is_empty() {
                    cur_q_opts.extend(opts);
                    continue;
                }
            }

            // 5. 题干行
            if let Some(cap) = re_stem.captures(line) {
                if let (Some(num_m), Some(stem_m)) = (cap.get(1), cap.get(2)) {
                    if let Ok(num) = num_m.as_str().parse::<u32>() {
                        if Self::should_stop_at_question(mode, last_seen_q_num, num) {
                            break;
                        }
                        // 保存上一题
                        flush_current_question(
                            &mut cur_q_num,
                            &mut cur_q_stem,
                            &mut cur_q_opts,
                            &mut cur_q_ans,
                            &mut cur_q_expl,
                            &mut cur_questions,
                            &end_answers,
                        );
                        last_seen_q_num = Some(num);
                        cur_q_num = Some(num);
                        cur_q_stem = stem_m.as_str().to_string();
                    }
                }
                continue;
            }

            // 6. 普通正文延伸（可能是跨行的题干或跨行的解析）
            if cur_q_num.is_some() {
                if let Some(expl) = cur_q_expl.as_mut() {
                    expl.push(' ');
                    expl.push_str(line);
                } else if cur_q_opts.is_empty() {
                    cur_q_stem.push(' ');
                    cur_q_stem.push_str(line);
                }
            }
        }

        // 保存最后一题
        flush_current_question(
            &mut cur_q_num,
            &mut cur_q_stem,
            &mut cur_q_opts,
            &mut cur_q_ans,
            &mut cur_q_expl,
            &mut cur_questions,
            &end_answers,
        );

        if !cur_questions.is_empty() {
            let title = if cur_mat_title.is_empty() {
                format!("材料 {}", groups.len() + 1)
            } else {
                cur_mat_title
            };
            groups.push((title, cur_questions));
        }

        if groups.is_empty() {
            return Vec::new();
        }

        let groups = mode.apply_grouping(groups, known_material_count);
        Self::build_material_exercises(groups)
    }

    fn build_material_exercises(
        groups: Vec<(String, Vec<ExerciseQuestion>)>,
    ) -> Vec<MaterialExercise> {
        groups
            .into_iter()
            .enumerate()
            .map(|(i, (title, questions))| MaterialExercise::new(i as u32 + 1, title, questions))
            .collect()
    }

    fn collect_end_answers(lines: &[&str], re_end_answer_block: &Regex) -> HashMap<u32, String> {
        let re_item_ans = Regex::new(r"(\d+)[\.、\s\:\-]+([A-Ga-g])").unwrap();
        let mut end_answers: HashMap<u32, String> = HashMap::new();
        let mut in_end_answer_block = false;

        for line in lines {
            if line.is_empty() {
                continue;
            }
            if re_end_answer_block.is_match(line) {
                in_end_answer_block = true;
                continue;
            }
            if in_end_answer_block {
                for cap in re_item_ans.captures_iter(line) {
                    if let (Some(num_m), Some(ans_m)) = (cap.get(1), cap.get(2)) {
                        if let Ok(num) = num_m.as_str().parse::<u32>() {
                            // 保留首次出现的答案，避免全卷末尾笔试重编题号（如 1-15）覆盖听力答案
                            end_answers
                                .entry(num)
                                .or_insert_with(|| ans_m.as_str().to_ascii_uppercase());
                        }
                    }
                }
            }
        }
        end_answers
    }

    fn is_section_instruction(line: &str) -> bool {
        (line.contains("听下面五段对话")
            || line.contains("听下面5段对话")
            || line.contains("听下面几段材料")
            || line.contains("听下面5段对话或独白"))
            && !line.contains("回答第")
    }

    fn should_stop_at_question(mode: ExerciseMode, last_seen: Option<u32>, num: u32) -> bool {
        let Some(prev) = last_seen else {
            return false;
        };
        if let Some(max_q) = mode.max_question_num() {
            if num > max_q || num <= prev {
                return true;
            }
        }
        false
    }

    /// 将一组题目按已知材料数均分或平铺
    pub(crate) fn split_questions_evenly(
        questions: Vec<ExerciseQuestion>,
        material_count: usize,
    ) -> Vec<(String, Vec<ExerciseQuestion>)> {
        if questions.is_empty() {
            return Vec::new();
        }
        let mat_count = material_count.max(1);
        if mat_count == 1 {
            return vec![("材料 1".to_string(), questions)];
        }

        // 尝试均匀切分
        let total = questions.len();
        let chunk_size = total.div_ceil(mat_count);
        let mut out = Vec::new();
        let mut iter = questions.into_iter();

        for i in 1..=mat_count {
            let cur_chunk: Vec<ExerciseQuestion> = iter.by_ref().take(chunk_size).collect();
            if cur_chunk.is_empty() {
                break;
            }
            out.push((format!("材料 {i}"), cur_chunk));
        }

        out
    }

    /// 从单行文本中精确切分出一个或多个 A-G 选项
    fn extract_options_from_line(line: &str) -> Vec<String> {
        // 选项头正则：行首或空格后，跟 [A-G] 或 (A-G)，再跟点/括号/顿号/冒号
        let re_head = Regex::new(
            r"(?i)(?:^|[\s\t]+)(?:(?:\(|（)?([A-G])(?:\)|）|[\.、:：])|([A-G])[\.、:：])\s*",
        )
        .unwrap();

        let matches: Vec<_> = re_head.find_iter(line).collect();
        if matches.is_empty() {
            return Vec::new();
        }

        // 第一个匹配项必须在比较靠近行首的位置（允许少量缩进）
        if matches[0].start() > 4 {
            return Vec::new();
        }

        let mut opts = Vec::new();
        for i in 0..matches.len() {
            let cur_m = &matches[i];
            let next_start = if i + 1 < matches.len() {
                matches[i + 1].start()
            } else {
                line.len()
            };

            // 提取选项标签字母（如 'A'）
            let matched_str = cur_m.as_str().trim();
            let key = matched_str
                .chars()
                .find(|c| c.is_ascii_alphabetic())
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('A');

            let content = line[cur_m.end()..next_start].trim();
            opts.push(format!("{key}. {content}"));
        }

        opts
    }
}

/// 归一化复制粘贴文本：统一换行与空白，折叠影响结构识别的全角字符，
/// 并清除零宽等不可见格式符。正文中的中文标点保持原样。
pub(crate) fn sanitize_input(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for c in normalized.chars() {
        if c == '\n' || matches!(c, '\u{0085}' | '\u{2028}' | '\u{2029}') {
            out.push('\n');
        } else if is_invisible_format(c) {
            continue;
        } else if c.is_whitespace() {
            out.push(' ');
        } else {
            out.push(fold_structural_fullwidth(c));
        }
    }
    out
}

/// 仅折叠题号/选项/答案等结构位置可能出现的全角字符，
/// 避免像 NFKC 那样连正文问号、逗号等一并改写。
fn fold_structural_fullwidth(c: char) -> char {
    match c {
        '\u{FF10}'..='\u{FF19}' | '\u{FF21}'..='\u{FF3A}' | '\u{FF41}'..='\u{FF5A}' => {
            char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
        }
        '\u{FF0D}' => '-',
        '\u{FF0E}' => '.',
        '\u{FF1A}' => ':',
        '\u{FF08}' => '(',
        '\u{FF09}' => ')',
        _ => c,
    }
}

/// 复制粘贴常见的零宽/格式控制/变体选择符：无字形但会打断正则锚点。
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_standard_text_with_materials() {
        let text = r#"
Text 1
1. Where does the conversation probably take place?
A. In a library.
B. In a bookstore.
C. In a restaurant.
答案: A
解析: 女士提到了图书借阅证。

Text 2
2. What time will the train leave?
A. At 8:30.  B. At 9:00.  C. At 9:30.
Key: B
"#;

        let result = ExerciseParser::parse(text, 2);
        assert_eq!(result.len(), 2);

        // 材料 1
        assert_eq!(result[0].material_ordinal, 1);
        assert_eq!(result[0].title, "Text 1");
        assert_eq!(result[0].questions.len(), 1);
        let q1 = &result[0].questions[0];
        assert_eq!(q1.num, 1);
        assert_eq!(q1.stem, "Where does the conversation probably take place?");
        assert_eq!(q1.options.len(), 3);
        assert_eq!(q1.options[0], "A. In a library.");
        assert_eq!(q1.answer.as_deref(), Some("A"));
        assert!(q1.explanation.as_ref().unwrap().contains("图书借阅证"));

        // 材料 2
        assert_eq!(result[1].material_ordinal, 2);
        assert_eq!(result[1].title, "Text 2");
        assert_eq!(result[1].questions.len(), 1);
        let q2 = &result[1].questions[0];
        assert_eq!(q2.num, 2);
        assert_eq!(q2.stem, "What time will the train leave?");
        assert_eq!(q2.options.len(), 3);
        assert_eq!(q2.options[1], "B. At 9:00.");
        assert_eq!(q2.answer.as_deref(), Some("B"));
    }

    #[test]
    fn test_parse_end_answer_block() {
        let text = r#"
1. What does the man want to buy?
A. A shirt.
B. A coat.
2. How much is it?
A. $20.
B. $30.

【参考答案】
1. A  2. B
"#;

        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].questions.len(), 2);
        assert_eq!(result[0].questions[0].answer.as_deref(), Some("A"));
        assert_eq!(result[0].questions[1].answer.as_deref(), Some("B"));
    }

    #[test]
    fn test_parse_json_format() {
        let json = r#"[
            {
                "material_ordinal": 1,
                "title": "Passage 1",
                "questions": [
                    {
                        "num": 1,
                        "stem": "Who is the speaker?",
                        "options": ["A. A teacher", "B. A student"],
                        "answer": "A"
                    }
                ]
            }
        ]"#;

        let result = ExerciseParser::parse(json, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Passage 1");
        assert_eq!(result[0].questions[0].stem, "Who is the speaker?");
        assert_eq!(result[0].questions[0].answer.as_deref(), Some("A"));
    }

    #[test]
    fn test_parse_chinese_punctuation_and_parentheses() {
        let text = r#"
听力材料一
1、这个人在做什么？
(A) 看书 (B) 睡觉 (C) 跑步
答案：C
解析：听到脚步声。
"#;
        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].questions.len(), 1);
        let q = &result[0].questions[0];
        assert_eq!(q.num, 1);
        assert_eq!(q.stem, "这个人在做什么？");
        assert_eq!(q.options.len(), 3);
        assert_eq!(q.options[0], "A. 看书");
        assert_eq!(q.options[1], "B. 睡觉");
        assert_eq!(q.options[2], "C. 跑步");
        assert_eq!(q.answer.as_deref(), Some("C"));
        assert_eq!(q.explanation.as_deref(), Some("听到脚步声。"));
    }

    #[test]
    fn test_group_without_material_header_using_known_count() {
        let text = r#"
1. Question one
A. Opt 1   B. Opt 2
2. Question two
A. Opt 1   B. Opt 2
3. Question three
A. Opt 1   B. Opt 2
4. Question four
A. Opt 1   B. Opt 2
"#;
        // 已知音频切分出了 2 个材料
        let result = ExerciseParser::parse(text, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].material_ordinal, 1);
        assert_eq!(result[0].questions.len(), 2);
        assert_eq!(result[1].material_ordinal, 2);
        assert_eq!(result[1].questions.len(), 2);
    }

    #[test]
    fn test_parse_with_bom_and_zero_width_chars() {
        let text = "\u{feff}Text 1\u{200b}\n\
                    1\u{200b}. What is the woman doing?\n\
                    A\u{200b}. Driving a car.\n\
                    B\u{200b}. Walking in the park.\n\
                    答案\u{200b}: B\n\
                    解析\u{200b}: 女士在公园散步。";

        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Text 1");
        assert_eq!(result[0].questions.len(), 1);
        let q = &result[0].questions[0];
        assert_eq!(q.num, 1);
        assert_eq!(q.stem, "What is the woman doing?");
        assert_eq!(
            q.options,
            vec!["A. Driving a car.", "B. Walking in the park."]
        );
        assert_eq!(q.answer.as_deref(), Some("B"));
        assert_eq!(q.explanation.as_deref(), Some("女士在公园散步。"));
    }

    #[test]
    fn test_parse_fullwidth_digits_and_letters() {
        let text = "Ｔｅｘｔ　１\n\
                    １．女士在做什么？\n\
                    Ａ．看书　Ｂ．睡觉　Ｃ．跑步\n\
                    答案：Ｃ\n\
                    解析：听到脚步声。";

        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Text 1");
        assert_eq!(result[0].questions.len(), 1);
        let q = &result[0].questions[0];
        assert_eq!(q.num, 1);
        assert_eq!(q.stem, "女士在做什么？");
        assert_eq!(q.options, vec!["A. 看书", "B. 睡觉", "C. 跑步"]);
        assert_eq!(q.answer.as_deref(), Some("C"));
        assert_eq!(q.explanation.as_deref(), Some("听到脚步声。"));
    }

    #[test]
    fn test_parse_nbsp_crlf_and_unicode_line_separators() {
        let text = "Text\u{00a0}1\r\n\
                    1.\u{00a0}Where are the speakers?\u{2028}\
                    A.\u{00a0}In a library.\r\n\
                    B.\u{00a0}In a bank.\u{2028}\
                    Key:\u{00a0}A";

        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].questions.len(), 1);
        let q = &result[0].questions[0];
        assert_eq!(q.stem, "Where are the speakers?");
        assert_eq!(q.options, vec!["A. In a library.", "B. In a bank."]);
        assert_eq!(q.answer.as_deref(), Some("A"));
    }

    #[test]
    fn test_parse_json_with_bom() {
        let json = "\u{feff}[{\"material_ordinal\":1,\"title\":\"Passage 1\",\"questions\":[{\"num\":1,\"stem\":\"Who is the speaker?\",\"options\":[\"A. A teacher\",\"B. A student\"],\"answer\":\"A\"}]}]";

        let result = ExerciseParser::parse(json, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Passage 1");
        assert_eq!(result[0].questions[0].stem, "Who is the speaker?");
        assert_eq!(result[0].questions[0].answer.as_deref(), Some("A"));
    }

    #[test]
    fn test_parse_chinese_standard_exam_listening_paper() {
        let text = r#"一. 听力练习
第一节（共5小题；每小题1分，满分5分）
1．What do we know about Jim?
A．He has a leg injury.									B．He withdrew from the football team.
C．He spent his holiday with his teammates.
2．What is the woman?
A．Working in sales.	B．Working with people.		C．Working with numbers.
3．What happened to the man last week?
A．He lost his job.	B．He bought a newspaper.	C．He refused his co-workers.
4．Where does the woman live now?
A．In Miami.	B．In New York.			C．In Los Angeles.
5．What probably is the man?
A．A teacher.	B．A reporter.			C．A student.
第二节（共10小题；每小题1.5分，满分15分）
听下面一段对话，回答第6至第8小题。
6．What did the woman feel scared of at university at the beginning?
A．Taking tests.	B．Leaving home.			C．Communicating with new people.
7．What is the woman studying at university?
A．English.	B．French.			C．Russian.
8．What does the man want to be?
A．A teacher.	B．A translator.			C．A historian.
听下面一段对话，回答第9至第11小题。
9．What happened to the man on his journey?
A．He was late arriving.									B．He couldn’t find his bags.	
C．He lost his way to the destination.
10．How long has the man been traveling?
A．For 17 hours.	B．For 20 hours.			C．For 24 hours.
11．Why did the hotel change the man’s room?
A．He paid to have a better room.						B．The hotel has made a mistake.
C．He has experienced a tough time.
听下面一段独白，回答第12至第15小题。
12．What did the speaker do before the year 2012?
A．A fitness coach.	       B．A chess player.			C．A marathon runner.
13．Why was the 2016 Olympics important for the speaker?
A．He was motivated by Bolt.  B．He broke a world record.	C．He won fifth place.
14．Which is the hardest for the speaker?
A．Getting over an injury.     B．Doing strength training 	C．Representing Botswana.
15．What is the speaker mainly talking about?
A．His plan to go for the gold.					B．His experience on the track.	
C．His love for his home country.
"#;
        // 未指定多材料时保留 4 个显式材料组
        let result = ExerciseParser::parse(text, 1);
        assert_eq!(result.len(), 4);
        assert!(result[0].title.contains("第一节"));
        assert_eq!(result[0].questions.len(), 5);
        assert_eq!(result[0].questions[0].num, 1);
        assert_eq!(result[0].questions[4].num, 5);

        assert_eq!(result[1].title, "听下面一段对话，回答第6至第8小题");
        assert_eq!(result[1].questions.len(), 3);
        assert_eq!(result[1].questions[0].num, 6);
        assert_eq!(result[1].questions[2].num, 8);

        assert_eq!(result[2].title, "听下面一段对话，回答第9至第11小题");
        assert_eq!(result[2].questions.len(), 3);
        assert_eq!(result[2].questions[0].num, 9);
        assert_eq!(result[2].questions[2].num, 11);

        assert_eq!(result[3].title, "听下面一段独白，回答第12至第15小题");
        assert_eq!(result[3].questions.len(), 4);
        assert_eq!(result[3].questions[0].num, 12);
        assert_eq!(result[3].questions[3].num, 15);

        // 当已知音频切分为 8 个材料时（前 5 题为单题短对话，后 3 段长材料），智能展开第一节
        let result_8 = ExerciseParser::parse(text, 8);
        assert_eq!(result_8.len(), 8);
        for i in 1..=5 {
            assert_eq!(result_8[i - 1].material_ordinal, i as u32);
            assert_eq!(result_8[i - 1].questions.len(), 1);
            assert_eq!(result_8[i - 1].questions[0].num, i as u32);
        }
        assert_eq!(result_8[5].material_ordinal, 6);
        assert_eq!(result_8[5].questions.len(), 3);
        assert_eq!(result_8[6].material_ordinal, 7);
        assert_eq!(result_8[6].questions.len(), 3);
        assert_eq!(result_8[7].material_ordinal, 8);
        assert_eq!(result_8[7].questions.len(), 4);
    }
}
