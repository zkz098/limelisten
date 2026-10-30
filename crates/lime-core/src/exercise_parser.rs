//! 智能听力题目解析器：支持纯文本/Markdown 启发式解析与 JSON 结构化解析。

use crate::{ExerciseQuestion, MaterialExercise};
use regex::Regex;
use std::collections::HashMap;

pub struct ExerciseParser;

impl ExerciseParser {
    /// 解析输入的文本或 JSON 字符串。
    /// `known_material_count` 是音频当前切分出的材料数（去除引言后），用于在未检测到显式材料头时智能分组。
    pub fn parse(input: &str, known_material_count: usize) -> Vec<MaterialExercise> {
        let trimmed = input.trim();
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
                return Self::group_questions_into_materials(questions, known_material_count);
            }
        }

        // 2. 启发式文本/Markdown 解析
        Self::parse_text(trimmed, known_material_count)
    }

    /// 启发式纯文本解析
    fn parse_text(text: &str, known_material_count: usize) -> Vec<MaterialExercise> {
        let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();

        // 正则表达式定义
        // 材料标题行，例如: "Text 1", "Passage 2", "Section A", "材料 1", "第一节"
        let re_mat = Regex::new(r"(?i)^(?:Text|Passage|Section|Conversation|材料|第[一二三四五六七八九十0-9]+节|听力材料)\s*([0-9A-Za-z一二三四五六七八九十]+)?.*$").unwrap();

        // 题干行，例如: "1. What is the woman doing?", "第 2 题: ...", "3、..."
        let re_stem = Regex::new(r"^(?:第\s*)?(\d+)[\.、\s\)\:\-]+\s*(.+)$").unwrap();

        // 答案行，例如: "答案: A", "Key: B", "[答案] C", "Answer: A"
        let re_answer = Regex::new(r"(?i)^(?:【?答案】?|【?Keys?】?|【?Answers?】?)[:：\s]*([A-Ga-g])\b").unwrap();

        // 解析行，例如: "解析: 女士提到了...", "Explanation: ..."
        let re_expl = Regex::new(r"(?i)^(?:【?解析】?|【?Explanation】?|【?Notes?】?)[:：\s]*(.+)$").unwrap();

        // 集中答案块特征行，例如: "【参考答案】", "Keys:"
        let re_end_answer_block = Regex::new(r"(?i)^[【\[]?(?:参考答案|答案|Keys?|Answers?)[】\]]?$").unwrap();

        // 集中答案格式：例如 "1. A  2. B" 或 "1-5 A B C D A"
        let re_item_ans = Regex::new(r"(\d+)[\.、\s\:\-]+([A-Ga-g])").unwrap();

        let mut end_answers: HashMap<u32, String> = HashMap::new();
        let mut in_end_answer_block = false;

        // 先扫描一遍提取文末集中答案
        for line in &lines {
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
                            end_answers.insert(num, ans_m.as_str().to_ascii_uppercase());
                        }
                    }
                }
            }
        }

        let mut groups: Vec<(String, Vec<ExerciseQuestion>)> = Vec::new();
        let mut cur_mat_title = String::new();
        let mut cur_questions: Vec<ExerciseQuestion> = Vec::new();

        let mut cur_q_num: Option<u32> = None;
        let mut cur_q_stem = String::new();
        let mut cur_q_opts: Vec<String> = Vec::new();
        let mut cur_q_ans: Option<String> = None;
        let mut cur_q_expl: Option<String> = None;

        let flush_current_question = |cur_num: &mut Option<u32>,
                                      cur_stem: &mut String,
                                      cur_opts: &mut Vec<String>,
                                      cur_ans: &mut Option<String>,
                                      cur_expl: &mut Option<String>,
                                      questions: &mut Vec<ExerciseQuestion>,
                                      end_ans_map: &HashMap<u32, String>| {
            if let Some(num) = *cur_num {
                if !cur_stem.is_empty() {
                    let mut q = ExerciseQuestion::new(num, cur_stem.trim(), std::mem::take(cur_opts));
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

            // 1. 判断是否是材料分割行
            // 条件：匹配 re_mat 且不包含题号特征，且长度不过长（<= 30 字符）
            if line.len() <= 30 && re_mat.is_match(line) && !re_stem.is_match(line) {
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
                cur_mat_title = line.to_string();
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

                if let (Some(num_m), Some(stem_m)) = (cap.get(1), cap.get(2)) {
                    if let Ok(num) = num_m.as_str().parse::<u32>() {
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

        // 若只解析出一个组但没有显式材料标题，且已知有多个材料，进行按题均分或平摊
        if groups.len() == 1 && groups[0].0 == "材料 1" && known_material_count > 1 {
            let (_, questions) = groups.remove(0);
            return Self::group_questions_into_materials(questions, known_material_count);
        }

        // 构建带有连续 material_ordinal 的 MaterialExercise 列表
        groups
            .into_iter()
            .enumerate()
            .map(|(i, (title, questions))| {
                MaterialExercise::new(i as u32 + 1, title, questions)
            })
            .collect()
    }

    /// 将一组题目按已知材料数均分或平铺
    fn group_questions_into_materials(
        questions: Vec<ExerciseQuestion>,
        material_count: usize,
    ) -> Vec<MaterialExercise> {
        if questions.is_empty() {
            return Vec::new();
        }
        let mat_count = material_count.max(1);
        if mat_count == 1 {
            return vec![MaterialExercise::new(1, "材料 1", questions)];
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
            out.push(MaterialExercise::new(i as u32, format!("材料 {i}"), cur_chunk));
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
}
