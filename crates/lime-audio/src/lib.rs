//! 音频引擎：symphonia 解码 → signalsmith-stretch 变速 → 环形缓冲 → cpal 输出。
//! 设计依据 docs/PLAN.md §7（方案 A：自建 Transport，帧级控制 seek/循环/变速）。
//! 待填：Player 主体（见 PLAN §7）。

use lime_core::{Error, Result};
use std::path::Path;

pub mod decode;
pub mod engine;
pub mod render;
pub mod resample;
pub mod stretch;

pub use engine::{Engine, Shared};
pub use render::{render, write_wav, RenderOptions};

/// 音频文件基本信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
}

/// 用 symphonia probe 读取时长/采样率/声道数（不解码全文件）。
pub fn probe(path: &Path) -> Result<AudioInfo> {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).map_err(|e| Error::Io(e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(e.to_string()))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| Error::Decode("no default track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| Error::Decode("no audio codec parameters".into()))?;
    let sample_rate = params.sample_rate.unwrap_or(44_100);
    let channels = params
        .channels
        .as_ref()
        .map(|channels| channels.count() as u16)
        .unwrap_or(2);
    let duration_ms = match (track.time_base, track.duration) {
        (Some(time_base), Some(duration)) => time_base
            .calc_duration(duration)
            .map(|time| time.as_millis().max(0) as u64)
            .unwrap_or(0),
        _ => track
            .num_frames
            .map(|frames| frames.saturating_mul(1000) / sample_rate as u64)
            .unwrap_or(0),
    };
    Ok(AudioInfo {
        sample_rate,
        channels,
        duration_ms,
    })
}
