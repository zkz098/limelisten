//! 解码：symphonia → 交错 f32，支持 seek（用于单题循环与题间跳转）。

use lime_core::{Error, Result};
use std::path::Path;

pub struct Decoder {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    pub sample_rate: u32,
    pub channels: usize,
    buf: Vec<f32>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self> {
        use symphonia::core::codecs::DecoderOptions;
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
        let format = probed.format;
        let track = format
            .default_track()
            .ok_or_else(|| Error::Decode("no default track".into()))?;
        let track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| Error::Decode(e.to_string()))?;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count())
            .unwrap_or(2);
        Ok(Self { format, decoder, track_id, sample_rate, channels, buf: Vec::new() })
    }

    /// 解出一批交错 f32 样本；返回 None 表示到达文件末尾。
    pub fn next_packet(&mut self) -> Result<Option<&[f32]>> {
        use symphonia::core::errors::Error as SymErr;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None)
                }
                Err(e) => return Err(Error::Decode(e.to_string())),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    use symphonia::core::audio::Signal;
                    // 类型无关：先造一个同规格的 f32 缓冲再转换（支持 mp3/flac/wav 各种位深）
                    let mut dest = audio.make_equivalent::<f32>();
                    audio.convert(&mut dest);
                    self.buf.clear();
                    let frames = dest.frames();
                    let ch = dest.spec().channels.count();
                    self.buf.reserve(frames * ch);
                    if ch == 1 {
                        self.buf.extend(dest.chan(0).iter().copied());
                    } else {
                        for f in 0..frames {
                            for c in 0..ch {
                                self.buf.push(dest.chan(c)[f]);
                            }
                        }
                    }
                    return Ok(Some(&self.buf));
                }
                Err(SymErr::DecodeError(_)) => continue,
                Err(e) => return Err(Error::Decode(e.to_string())),
            }
        }
    }

    /// 按毫秒跳转（mp3 为帧级精度，实测 ~26 ms）。
    pub fn seek_ms(&mut self, ms: u64) -> Result<()> {
        use symphonia::core::formats::SeekMode;
        use symphonia::core::formats::SeekTo;
        let time = symphonia::core::units::Time::from(ms as f64 / 1000.0);
        self.format
            .seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) })
            .map_err(|e| Error::Decode(e.to_string()))?;
        self.decoder.reset();
        Ok(())
    }
}
