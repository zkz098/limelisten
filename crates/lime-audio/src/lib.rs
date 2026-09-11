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
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| Error::Io(e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| Error::Decode(e.to_string()))?;
    let track = probed
        .format
        .default_track()
        .ok_or_else(|| Error::Decode("no default track".into()))?;
    let params = &track.codec_params;
    let sample_rate = params.sample_rate.unwrap_or(44_100);
    let channels = params.channels.map(|c| c.count() as u16).unwrap_or(2);
    let duration_ms = match (params.n_frames, params.time_base) {
        (Some(frames), Some(tb)) => {
            let t = tb.calc_time(frames);
            (t.seconds * 1000 + (t.frac * 1000.0) as u64) as u64
        }
        (Some(frames), None) => frames * 1000 / sample_rate as u64,
        _ => 0,
    };
    Ok(AudioInfo { sample_rate, channels, duration_ms })
}
