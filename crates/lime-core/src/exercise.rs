use serde::{Deserialize, Serialize};

/// 单道听力选择题
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExerciseQuestion {
    /// 题号（从 1 开始）
    pub num: u32,
    /// 题干文本
    pub stem: String,
    /// 选项列表，如 `["A. In a library", "B. In a restaurant", "C. In a bookstore"]`
    pub options: Vec<String>,
    /// 标准答案（可选，如 `"A"`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// 答案解析（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

impl ExerciseQuestion {
    pub fn new(num: u32, stem: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            num,
            stem: stem.into(),
            options,
            answer: None,
            explanation: None,
        }
    }

    pub fn with_answer(mut self, answer: impl Into<String>) -> Self {
        self.answer = Some(answer.into());
        self
    }

    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }
}

/// 归属于单份材料（Material）的一组题目
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialExercise {
    /// 关联的材料序号（从 1 开始，对应 ChapterLevel::Material 的 ordinal）
    pub material_ordinal: u32,
    /// 题目组的标题（如 "Text 1" 或 "第一节"）
    #[serde(default)]
    pub title: String,
    /// 该材料下的全部题目
    pub questions: Vec<ExerciseQuestion>,
}

impl MaterialExercise {
    pub fn new(material_ordinal: u32, title: impl Into<String>, questions: Vec<ExerciseQuestion>) -> Self {
        Self {
            material_ordinal,
            title: title.into(),
            questions,
        }
    }
}
