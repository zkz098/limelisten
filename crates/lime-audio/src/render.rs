//! 离线渲染：解码 →（重采样）→ 变速 → 收集为样本。
//!
//! 两个用途：
//! 1. 无 UI 的自动化验收（验证“变速不变调”）
//! 2. P2 生词本的“原声切片”导出（截取某句音频）

use crate::decode::Decoder;
use crate::resample::LinearResampler;
use crate::stretch::{Stretcher, BLOCK_FRAMES};
use lime_core::{Error, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub speed: f32,
    pub target_rate: u32,
    pub channels: usize,
    /// 最多渲染多少毫秒（None = 整段）
    pub max_ms: Option<u64>,
    /// 起始位置（毫秒）
    pub start_ms: u64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            speed: 1.0,
            target_rate: 48_000,
            channels: 2,
            max_ms: None,
            start_ms: 0,
        }
    }
}

/// 渲染为交错的 f32（设备采样率、指定声道数）。
pub fn render(path: &Path, opt: &RenderOptions) -> Result<Vec<f32>> {
    let mut dec = Decoder::open(path)?;
    let src_rate = dec.sample_rate;
    let src_ch = dec.channels;
    if opt.start_ms > 0 {
        dec.seek_ms(opt.start_ms)?;
    }
    let rate_scale = src_rate as f64 / opt.target_rate as f64;
    let mut stretch = Stretcher::new(opt.channels, opt.target_rate);
    stretch.set_speed(opt.speed);
    let mut resampler = if src_rate != opt.target_rate {
        Some(LinearResampler::new(src_rate, opt.target_rate, opt.channels))
    } else {
        None
    };

    let mut out_frames_total: u64 = 0;
    let max_frames = opt
        .max_ms
        .map(|ms| (ms as f64 / 1000.0 * opt.target_rate as f64) as u64);

    let mut out: Vec<f32> = Vec::new();
    // 跳过启动预热（与实时播放路径保持一致，否则离线结果会多出开头的高频垃圾）
    let mut skip_out: usize = if std::env::var_os("LIME_NO_WARMUP_SKIP").is_some() {
        0
    } else {
        stretch.warmup_frames()
    };
    loop {
        let mut src_block: Vec<f32> = Vec::new();
        while src_block.len() < BLOCK_FRAMES * src_ch {
            match dec.next_packet()? {
                Some(buf) => src_block.extend_from_slice(buf),
                None => break,
            }
        }
        if src_block.is_empty() {
            break;
        }
        let frames = src_block.len() / src_ch;

        // 声道归一化
        let mut norm: Vec<f32> = Vec::with_capacity(frames * opt.channels);
        if src_ch == opt.channels {
            norm = src_block;
        } else if src_ch == 1 {
            for f in 0..frames {
                let v = src_block[f];
                for _ in 0..opt.channels {
                    norm.push(v);
                }
            }
        } else {
            for f in 0..frames {
                for c in 0..opt.channels {
                    norm.push(src_block[f * src_ch + c.min(src_ch - 1)]);
                }
            }
        }

        let at_rate: Vec<f32> = if let Some(r) = resampler.as_mut() {
            let n = ((frames as f64) / rate_scale).round() as usize;
            let mut y = vec![0.0f32; n.max(1) * opt.channels];
            r.process(&norm, &mut y);
            y
        } else {
            norm
        };

        let mut chunk = stretch.process(&at_rate).to_vec();
        if skip_out > 0 {
            let n_frames = chunk.len() / opt.channels;
            let drop_frames = skip_out.min(n_frames);
            skip_out -= drop_frames;
            chunk.drain(..drop_frames * opt.channels);
        }
        out_frames_total += (chunk.len() / opt.channels) as u64;
        out.extend_from_slice(&chunk);
        if let Some(mf) = max_frames {
            if out_frames_total >= mf {
                break;
            }
        }
    }
    // 收尾
    let tail = stretch.latency_frames().max(BLOCK_FRAMES);
    let mut t = vec![0.0f32; tail * opt.channels];
    stretch.flush(&mut t);
    out.extend_from_slice(&t);
    Ok(out)
}

/// 流式 16-bit PCM WAV 写入器。
///
/// 与 `write_wav` 不同，它**逐块写盘**，不需要把整段样本留在内存里——
/// 这正是把长音频转成 16 kHz 转写输入时该走的路子（15 分钟素材可达数十 MB）。
pub struct WavStreamWriter {
    inner: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl WavStreamWriter {
    pub fn create(path: &Path, channels: u16, rate: u32) -> Result<Self> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let inner = hound::WavWriter::create(path, spec).map_err(|e| Error::Io(e.to_string()))?;
        Ok(Self { inner })
    }

    /// 追加一段交错 f32（自动 clamp 到 16-bit PCM）。
    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            self.inner.write_sample(v).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.inner.finalize().map_err(|e| Error::Io(e.to_string()))
    }
}

/// 写 16-bit PCM WAV（给自动化验收与“原声切片”用）。
pub fn write_wav(path: &Path, samples: &[f32], channels: u16, rate: u32) -> Result<()> {
    let mut w = WavStreamWriter::create(path, channels, rate)?;
    w.write(samples)?;
    w.finish()
}

/// 用零交叉率估主频（自动化验收用，避免引入 FFT 依赖）。
pub fn dominant_freq_zero_cross(x: &[f32], rate: u32) -> f32 {
    let mut zc = 0usize;
    for w in x.windows(2) {
        if (w[0] <= 0.0) != (w[1] <= 0.0) {
            zc += 1;
        }
    }
    zc as f32 * rate as f32 / (2.0 * x.len().max(1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 流式写入应等价于一次性写入（写盘格式与样本值一致）。
    #[test]
    fn stream_writer_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lime_stream_wav_{}.wav", std::process::id()));
        let src: Vec<f32> = (0..1000).map(|i| (i as f32 / 1000.0) * 1.6 - 0.8).collect();
        {
            let mut w = WavStreamWriter::create(&path, 1, 16_000).expect("create");
            for chunk in src.chunks(64) {
                w.write(chunk).expect("write");
            }
            w.finish().expect("finish");
        }
        let mut r = hound::WavReader::open(&path).expect("open");
        assert_eq!(r.spec().sample_rate, 16_000);
        assert_eq!(r.spec().channels, 1);
        let got: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(got.len(), src.len());
        for (a, b) in src.iter().zip(got.iter()) {
            assert_eq!((a.clamp(-1.0, 1.0) * 32767.0) as i16, *b);
        }
        let _ = std::fs::remove_file(&path);
    }
}
