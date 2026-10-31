//! 解码：symphonia → 交错 f32，支持 seek（用于单题循环与题间跳转）。

use lime_core::{Error, Result};
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    pub sample_rate: u32,
    pub channels: usize,
    buf: Vec<f32>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
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
        let track_id = track.id;
        let codec_params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| Error::Decode("no audio codec parameters".into()))?;
        // 0.5 的 gapless 在 FormatOptions 上且默认关闭；0.6 移到解码器且默认开启。
        // 保持关闭，避免解码时间轴相对旧版偏移、与已缓存的章节/字幕失配。
        let decoder_opts = AudioDecoderOptions::default().gapless(false);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(codec_params, &decoder_opts)
            .map_err(|e| Error::Decode(e.to_string()))?;
        let sample_rate = codec_params.sample_rate.unwrap_or(44_100);
        let channels = codec_params
            .channels
            .as_ref()
            .map(|channels| channels.count())
            .unwrap_or(2);
        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            buf: Vec::new(),
        })
    }

    /// 解出一批交错 f32 样本；返回 None 表示到达文件末尾。
    pub fn next_packet(&mut self) -> Result<Option<&[f32]>> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None)
                }
                Err(e) => return Err(Error::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    // 类型无关：解码器统一转换为交错 f32（支持 mp3/flac/wav 各种位深）。
                    audio.copy_to_vec_interleaved(&mut self.buf);
                    return Ok(Some(&self.buf));
                }
                Err(SymErr::DecodeError(_)) => continue,
                Err(e) => return Err(Error::Decode(e.to_string())),
            }
        }
    }

    /// 按毫秒跳转（mp3 为帧级精度，实测 ~26 ms）。
    pub fn seek_ms(&mut self, ms: u64) -> Result<()> {
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::from_millis_u64(ms),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| Error::Decode(e.to_string()))?;
        self.decoder.reset();
        Ok(())
    }
}
