//! `.limed` 预切分与转译缓存文件格式规范与编解码。
//!
//! 格式规范：
//! - 前 8 字节文件头：
//!   - 0..5: `b"LIMED"` (魔数 ASCII 字符串)
//!   - 5: `0x01` (主版本号 1)
//!   - 6: `0x01` (压缩算法：1 = zstd, 0 = 原始 json)
//!   - 7: `0x00` (保留位)
//! - 8..: zstd 压缩的 UTF-8 JSON 数据包（包含 LimedFile 结构体）。

use crate::{Chapter, Error, Result, Sentence};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LIMED_MAGIC: &[u8; 5] = b"LIMED";
pub const LIMED_VERSION: u8 = 1;
pub const COMPRESSION_ZSTD: u8 = 1;
pub const COMPRESSION_NONE: u8 = 0;
pub const HEADER_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimedMeta {
    pub audio_filename: String,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_generator")]
    pub generator: String,
    /// 生成字幕时使用的转写管线指纹（见 `lime_asr::pipeline_fingerprint`）。
    ///
    /// 空串 = 老缓存（没有指纹）或外部工具生成——调用方比对不一致时应**保留章节、
    /// 丢弃字幕**，避免继续展示旧解码参数（如 `-mc -1` 的重复幻觉）产生的文本。
    #[serde(default)]
    pub asr_fp: String,
}

fn default_version() -> u32 {
    1
}

fn default_generator() -> String {
    "limelisten".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimedFile {
    pub meta: LimedMeta,
    pub chapters: Vec<Chapter>,
    pub sentences: Vec<Sentence>,
}

impl LimedFile {
    pub fn new(meta: LimedMeta, chapters: Vec<Chapter>, sentences: Vec<Sentence>) -> Self {
        Self { meta, chapters, sentences }
    }

    /// 编码为带 8 字节魔数头与 zstd 压缩的二进制格式。
    pub fn encode(&self) -> Result<Vec<u8>> {
        let json_bytes = serde_json::to_vec(self)
            .map_err(|e| Error::Limed(format!("json encode failed: {e}")))?;
        let compressed = zstd::encode_all(&json_bytes[..], 3)
            .map_err(|e| Error::Limed(format!("zstd compress failed: {e}")))?;

        let mut out = Vec::with_capacity(HEADER_LEN + compressed.len());
        out.extend_from_slice(LIMED_MAGIC);
        out.push(LIMED_VERSION);
        out.push(COMPRESSION_ZSTD);
        out.push(0); // reserved
        out.extend_from_slice(&compressed);
        Ok(out)
    }

    /// 解码 `.limed` 二进制数据。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Limed(format!(
                "file too short: {} bytes (expected at least {})",
                bytes.len(),
                HEADER_LEN
            )));
        }

        if &bytes[0..5] != LIMED_MAGIC {
            return Err(Error::Limed("invalid magic header, not a .limed file".into()));
        }

        let version = bytes[5];
        if version != LIMED_VERSION {
            return Err(Error::Limed(format!(
                "unsupported limed version: {version} (expected {LIMED_VERSION})"
            )));
        }

        let comp = bytes[6];
        let payload = &bytes[HEADER_LEN..];

        let json_bytes = match comp {
            COMPRESSION_ZSTD => zstd::decode_all(payload)
                .map_err(|e| Error::Limed(format!("zstd decompression failed: {e}")))?,
            COMPRESSION_NONE => payload.to_vec(),
            other => {
                return Err(Error::Limed(format!("unknown compression type: {other}")));
            }
        };

        serde_json::from_slice(&json_bytes)
            .map_err(|e| Error::Limed(format!("json deserialize failed: {e}")))
    }

    /// 保存到文件。
    pub fn save(&self, path: &Path) -> Result<()> {
        let data = self.encode()?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, data).map_err(|e| Error::Io(format!("save {}: {e}", path.display())))
    }

    /// 从文件读取并解码。
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::Io(format!("read {}: {e}", path.display())))?;
        Self::decode(&bytes)
    }

    /// 校验是否与指定音频特征匹配。
    pub fn matches_audio(&self, duration_ms: u64, file_size: Option<u64>) -> bool {
        if duration_ms > 0 && self.meta.duration_ms > 0 {
            // 允许 ±1.5s 容器/解码器填充帧容差
            if self.meta.duration_ms.abs_diff(duration_ms) > 1500 {
                return false;
            }
        }

        if let Some(actual_size) = file_size {
            if self.meta.file_size > 0 && actual_size > 0 && self.meta.file_size != actual_size {
                // 如果文件大小不一致，但时长几乎完全一致（差 < 300ms），仍认为是匹配音频（可能修改了 ID3 标签）
                if self.meta.duration_ms.abs_diff(duration_ms) > 300 {
                    return false;
                }
            }
        }

        true
    }
}

/// 在指定音频文件同目录下，寻找并校验是否存在符合的 `.limed` 缓存文件。
pub fn find_matching_limed(
    audio_path: &Path,
    audio_duration_ms: u64,
    audio_size: Option<u64>,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();

    // 候选 1: audio_name.limed (如 "训练2.mp3" -> "训练2.limed")
    candidates.push(audio_path.with_extension("limed"));

    // 候选 2: audio_name.mp3.limed (如 "训练2.mp3" -> "训练2.mp3.limed")
    if let Some(file_name) = audio_path.file_name().and_then(|s| s.to_str()) {
        candidates.push(audio_path.with_file_name(format!("{file_name}.limed")));
    }

    for cand in candidates {
        if cand.is_file() {
            if let Ok(limed) = LimedFile::load(&cand) {
                if limed.matches_audio(audio_duration_ms, audio_size) {
                    return Some(cand);
                }
            }
        }
    }

    None
}

/// 当用户直接打开 `.limed` 文件时，尝试在同一目录下寻找对应的音频文件。
pub fn find_accompanying_audio(limed_path: &Path) -> Option<PathBuf> {
    const AUDIO_EXTS: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma"];

    let dir = limed_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = limed_path.file_stem()?.to_str()?;

    // 情况 1: "训练2.mp3.limed" -> stem 是 "训练2.mp3"
    let direct_candidate = dir.join(stem);
    if direct_candidate.is_file() {
        return Some(direct_candidate);
    }

    // 情况 2: "训练2.limed" -> stem 是 "训练2"，搜索 "训练2.<ext>"
    for ext in AUDIO_EXTS {
        let cand = dir.join(format!("{stem}.{ext}"));
        if cand.is_file() {
            return Some(cand);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChapterLevel, ChapterSource, Word};

    fn make_sample_limed() -> LimedFile {
        let meta = LimedMeta {
            audio_filename: "test_audio.mp3".into(),
            duration_ms: 60_000,
            sample_rate: 44100,
            channels: 2,
            file_size: 1_000_000,
            created_at: 1700000000,
            version: 1,
            generator: "limelisten_test".into(),
            asr_fp: "asr-pipeline/test".into(),
        };

        let chapters = vec![
            Chapter {
                seq: 0,
                level: ChapterLevel::Material,
                parent: None,
                ordinal: 1,
                start_ms: 0,
                end_ms: 30_000,
                title: "Material 1".into(),
                source: ChapterSource::Structure,
                confidence: 0.95,
                locked: false,
            },
            Chapter {
                seq: 1,
                level: ChapterLevel::Question,
                parent: Some(0),
                ordinal: 1,
                start_ms: 500,
                end_ms: 15_000,
                title: "Question 1".into(),
                source: ChapterSource::Asr,
                confidence: 0.92,
                locked: true,
            },
        ];

        let sentences = vec![Sentence {
            start_ms: 600,
            end_ms: 4500,
            text: "Welcome to the listening test.".into(),
            words: vec![
                Word { start_ms: 600, end_ms: 1100, text: "Welcome".into() },
                Word { start_ms: 1150, end_ms: 1300, text: "to".into() },
                Word { start_ms: 1350, end_ms: 1500, text: "the".into() },
                Word { start_ms: 1550, end_ms: 2200, text: "listening".into() },
                Word { start_ms: 2250, end_ms: 2800, text: "test.".into() },
            ],
            chapter: Some(1),
        }];

        LimedFile::new(meta, chapters, sentences)
    }

    #[test]
    fn test_limed_roundtrip() {
        let original = make_sample_limed();
        let encoded = original.encode().expect("encode should succeed");

        assert!(encoded.starts_with(LIMED_MAGIC));
        assert_eq!(encoded[5], LIMED_VERSION);
        assert_eq!(encoded[6], COMPRESSION_ZSTD);

        let decoded = LimedFile::decode(&encoded).expect("decode should succeed");
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_limed_invalid_magic() {
        let mut data = make_sample_limed().encode().unwrap();
        data[0] = b'X';
        let err = LimedFile::decode(&data).unwrap_err();
        match err {
            Error::Limed(msg) => assert!(msg.contains("invalid magic header")),
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn test_limed_unsupported_version() {
        let mut data = make_sample_limed().encode().unwrap();
        data[5] = 99; // invalid version
        let err = LimedFile::decode(&data).unwrap_err();
        match err {
            Error::Limed(msg) => assert!(msg.contains("unsupported limed version")),
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn test_limed_matches_audio() {
        let sample = make_sample_limed(); // duration 60_000, size 1_000_000
        assert!(sample.matches_audio(60_000, Some(1_000_000)));
        // 容差内 (差 800ms)
        assert!(sample.matches_audio(60_800, Some(1_000_000)));
        // 超出容差 (差 2000ms)
        assert!(!sample.matches_audio(62_000, Some(1_000_000)));
    }

    /// 老缓存没有 `seq` 字段：serde 会填 0，读取后交给 `normalize_chapters` 补序归位。
    #[test]
    fn test_legacy_limed_without_seq_still_loads() {
        let mut json: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&make_sample_limed()).unwrap()).unwrap();
        for c in json["chapters"].as_array_mut().unwrap() {
            c.as_object_mut().unwrap().remove("seq");
        }

        let decoded: LimedFile = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.chapters.len(), 2);
        assert_eq!(decoded.chapters[0].seq, 0);
        assert_eq!(decoded.chapters[1].seq, 0);

        let fixed = crate::normalize_chapters(&decoded.chapters);
        assert!(crate::chapters_are_canonical(&fixed));
        assert_eq!(fixed[1].seq, 1);
        assert_eq!(fixed[1].parent, Some(0));
    }

    /// 老缓存（或外部工具生成）没有 `asr_fp`：应能正常解析，指纹为空串，
    /// 交由调用方判定"字幕需要重新转写"（章节仍然可用）。
    #[test]
    fn test_legacy_limed_without_asr_fp_loads_empty() {
        let mut json: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&make_sample_limed()).unwrap()).unwrap();
        json["meta"].as_object_mut().unwrap().remove("asr_fp");

        let decoded: LimedFile = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.meta.asr_fp, "");
        assert_eq!(decoded.chapters.len(), 2);
        assert_eq!(decoded.sentences.len(), 1);
    }
}
