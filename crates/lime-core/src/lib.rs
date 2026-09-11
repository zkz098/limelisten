//! 领域模型：媒体、章节、句子、词、生词。无 IO 依赖。

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("asr: {0}")]
    Asr(String),
    #[error("store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    pub path: String,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// 两级切分：材料（Passage/Text）与题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterLevel {
    /// Passage / Text N —— 一整个对话或短文
    Material,
    /// 题号（按顺序推断，或来自念白正则）
    Question,
}

/// 章节来源：结构检测 / ASR 锚点 / 人工修正。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterSource {
    Structure,
    Asr,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub level: ChapterLevel,
    /// 父章节在列表中的索引（Question -> Material），Material 为 None。
    pub parent: Option<usize>,
    /// 同级序号，从 1 开始（即"第几题"）。
    pub ordinal: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
    pub source: ChapterSource,
    /// 0.0–1.0，低置信在 UI 标黄提示人工修正。
    pub confidence: f32,
    /// 人工锁定后重跑分析不覆盖。
    pub locked: bool,
}

impl Chapter {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sentence {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<Word>,
    /// 所属题章节索引（0-based，指向章节列表）。
    pub chapter: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VocabEntry {
    pub word: String,
    pub lemma: String,
    pub media_id: i64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub sentence: String,
    pub note: String,
}
