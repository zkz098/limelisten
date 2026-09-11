//! 播放引擎（PLAN §7 方案 A）：解码线程 → 变速 → 分块队列 → cpal 输出回调。
//!
//! 关键设计：
//! - **播放位置以“可听位置”为准**：源帧数(src_fed) 减去“已产出但未被回调消费”的帧数，
//!   否则 UI 上的时间轴会比耳朵早 ~200 ms，题间自动暂停也会停早。
//! - **gen（代际）+ ack**：暂停/跳转/循环时把队列里听不到的数据整批丢掉，
//!   否则“跳转”要等队列播完才生效（实测会拖 200 ms 以上）。
//! - 暂停 = 丢掉队列 + 记住可听位置；恢复 = seek 回该位置（mp3 帧级精度 ~26 ms）。
//! - 设备不接受文件采样率时才走线性重采样兜底（本机 WASAPI 接受 44.1k/48k，正常不触发）。

use crate::decode::Decoder;
use crate::resample::LinearResampler;
use crate::stretch::{Stretcher, BLOCK_FRAMES};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

use lime_core::{Error, Result};

/// 队列容量（块数）；每块约 `BLOCK_FRAMES` 帧。60 块 ≈ 1.3 s @48k。
const QUEUE_BLOCKS: usize = 60;

#[derive(Default)]
pub struct Shared {
    /// 可听位置（源时间轴，ms）
    pos_ms: AtomicU64,
    /// 是否正在出声
    playing: AtomicBool,
    /// 速度 ×1000
    speed_milli: AtomicU64,
    /// 队列代际：变化表示“丢弃已排队数据”
    gen: AtomicU64,
    ack_gen: AtomicU64,
    /// 已产出的输出帧 / 已被回调消费的输出帧（设备采样率）
    out_produced: AtomicU64,
    out_consumed: AtomicU64,
    /// 已喂入变速器的源帧数
    src_fed: AtomicU64,
    /// 自动暂停点（ms，0=无）
    stop_at_ms: AtomicU64,
    /// 到达自动暂停点（UI 轮询后清零）
    reached_stop: AtomicBool,
    /// A-B 循环
    loop_a_ms: AtomicU64,
    loop_b_ms: AtomicU64,
    loop_left: AtomicU64,
    /// 欠载次数（诊断用）
    underruns: AtomicU64,
    /// 时长（ms）
    duration_ms: AtomicU64,
    /// 播放到文件末尾
    eof: AtomicBool,
    finished: AtomicBool,
}

impl Shared {
    pub fn pos_ms(&self) -> u64 {
        self.pos_ms.load(Relaxed)
    }
    pub fn playing(&self) -> bool {
        self.playing.load(Relaxed)
    }
    pub fn speed(&self) -> f32 {
        self.speed_milli.load(Relaxed) as f32 / 1000.0
    }
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms.load(Relaxed)
    }
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Relaxed)
    }
    pub fn take_reached_stop(&self) -> bool {
        self.reached_stop.swap(false, Relaxed)
    }
    pub fn loop_left(&self) -> u64 {
        self.loop_left.load(Relaxed)
    }
    pub fn eof(&self) -> bool {
        self.eof.load(Relaxed)
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Relaxed)
    }
}

enum Cmd {
    Play,
    Pause,
    Seek(u64),
    SetSpeed(f32),
    Loop { a_ms: u64, b_ms: u64, count: u32 },
    StopAt(u64),
    Quit,
}

pub struct Engine {
    cmd: Sender<Cmd>,
    shared: Arc<Shared>,
    _stream: cpal::Stream,
    _worker: std::thread::JoinHandle<()>,
    pub device_rate: u32,
    pub device_channels: usize,
    pub resampling: bool,
    pub src_rate: u32,
}

impl Engine {
    /// 打开文件并建立输出流。`device_sample_rate`: None = 用文件的采样率（失败则退回默认配置）。
    pub fn new(path: &Path) -> Result<Self> {
        let info = crate::probe(path)?;
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| Error::Decode("no default output device".into()))?;
        let default_cfg = device
            .default_output_config()
            .map_err(|e| Error::Decode(format!("default_output_config: {e}")))?;

        // 优先用文件采样率（本机 WASAPI 支持 44.1k/48k）→ 避免重采样
        let want_rate = info.sample_rate;
        let (config, resampling) = if let Some(cfg) = pick_config(&device, want_rate) {
            (cfg, false)
        } else {
            (default_cfg.config(), true)
        };
        let device_rate = config.sample_rate;
        let device_channels = config.channels as usize;

        let shared = Arc::new(Shared::default());
        shared.speed_milli.store(1000, Relaxed);
        shared.duration_ms.store(info.duration_ms, Relaxed);

        let (tx, rx) = bounded::<(u64, Vec<f32>)>(QUEUE_BLOCKS);
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(32);

        let stream = build_stream(&device, config, shared.clone(), rx)?;
        stream
            .play()
            .map_err(|e| Error::Decode(format!("stream play: {e}")))?;

        let sh = shared.clone();
        let path_buf: PathBuf = path.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("lime-transport".into())
            .spawn(move || {
                if let Err(e) = worker_loop(&path_buf, &sh, tx, cmd_rx, device_rate, device_channels) {
                    eprintln!("[lime-audio] transport 线程结束: {e}");
                }
            })
            .map_err(|e| Error::Io(e.to_string()))?;

        Ok(Self {
            cmd: cmd_tx,
            shared,
            _stream: stream,
            _worker: worker,
            device_rate,
            device_channels,
            resampling,
            src_rate: info.sample_rate,
        })
    }

    pub fn play(&self) {
        let _ = self.cmd.send(Cmd::Play);
    }
    pub fn pause(&self) {
        let _ = self.cmd.send(Cmd::Pause);
    }
    pub fn toggle(&self) {
        if self.shared.playing() {
            self.pause()
        } else {
            self.play()
        }
    }
    pub fn seek_ms(&self, ms: u64) {
        let _ = self.cmd.send(Cmd::Seek(ms));
    }
    pub fn set_speed(&self, speed: f32) {
        let _ = self.cmd.send(Cmd::SetSpeed(speed));
    }
    /// 单题循环：`count` = 额外重复次数（0 = 不循环）。
    pub fn set_loop(&self, a_ms: u64, b_ms: u64, count: u32) {
        let _ = self.cmd.send(Cmd::Loop { a_ms, b_ms, count });
    }
    /// 播到 `ms` 自动暂停（题间暂停）。0 = 取消。
    pub fn set_stop_at(&self, ms: u64) {
        let _ = self.cmd.send(Cmd::StopAt(ms));
    }
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
    }
}

fn pick_config(device: &cpal::Device, rate: u32) -> Option<cpal::StreamConfig> {
    let ranges = device.supported_output_configs().ok()?;
    for r in ranges {
        if r.channels() >= 1 && rate >= r.min_sample_rate() && rate <= r.max_sample_rate() {
            // 用 F32 优先（我们的数据就是 f32）
            if r.sample_format() == cpal::SampleFormat::F32 {
                return Some(cpal::StreamConfig {
                    channels: r.channels(),
                    sample_rate: rate,
                    buffer_size: cpal::BufferSize::Default,
                });
            }
        }
    }
    None
}

fn build_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    rx: Receiver<(u64, Vec<f32>)>,
) -> Result<cpal::Stream> {
    match device.default_output_config().map(|c| c.sample_format()) {
        Ok(cpal::SampleFormat::F32) => build_typed::<f32>(device, config, shared, rx),
        Ok(cpal::SampleFormat::I16) => build_typed::<i16>(device, config, shared, rx),
        Ok(cpal::SampleFormat::U16) => build_typed::<u16>(device, config, shared, rx),
        _ => build_typed::<f32>(device, config, shared, rx),
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    rx: Receiver<(u64, Vec<f32>)>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::Sample + cpal::FromSample<f32> + Send + 'static,
{
    // 注意：回调参数是**交错样本**，而位置换算全用**帧**。
    // 这里不加 channel 归一化的话，立体声会把“已排队量”算成 2 倍，
    // 导致可听位置被钉在 0（实测变成 0.38 倍速）。
    let dev_ch = config.channels as usize;
    let mut cur: Vec<f32> = Vec::new();
    let mut cur_pos = 0usize;
    let mut local_gen = 0u64;
    let sh = shared.clone();
    let err_sh = shared.clone();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let gen = sh.gen.load(Relaxed);
                if gen != local_gen {
                    // 丢弃全部已排队数据（跳转/暂停/循环时用）
                    while rx.try_recv().is_ok() {}
                    cur.clear();
                    cur_pos = 0;
                    local_gen = gen;
                    sh.ack_gen.store(gen, Relaxed);
                }
                let mut i = 0usize;
                while i < data.len() {
                    if cur_pos >= cur.len() {
                        match rx.try_recv() {
                            Ok((g, v)) => {
                                if g != local_gen {
                                    continue;
                                }
                                cur = v;
                                cur_pos = 0;
                            }
                            Err(_) => {
                                for d in data[i..].iter_mut() {
                                    *d = T::from_sample(0.0f32);
                                }
                                sh.underruns.fetch_add(1, Relaxed);
                                break;
                            }
                        }
                    }
                    let n = (cur.len() - cur_pos).min(data.len() - i);
                    for k in 0..n {
                        data[i + k] = T::from_sample(cur[cur_pos + k]);
                    }
                    i += n;
                    cur_pos += n;
                    sh.out_consumed.fetch_add((n / dev_ch) as u64, Relaxed);
                }
            },
            move |e| eprintln!("[lime-audio] stream error: {e}"),
            None,
        )
        .map_err(|e| Error::Decode(format!("build_output_stream: {e}")))?;
    let _ = err_sh;
    Ok(stream)
}

/// 传输线程：解码 → （重采样）→ 变速 → 入队；同时负责跳转/循环/自动暂停。
fn worker_loop(
    path: &Path,
    shared: &Arc<Shared>,
    tx: Sender<(u64, Vec<f32>)>,
    cmd: Receiver<Cmd>,
    device_rate: u32,
    device_channels: usize,
) -> Result<()> {
    let mut dec = Decoder::open(path)?;
    let src_rate = dec.sample_rate;
    let src_ch = dec.channels;
    let rate_scale = src_rate as f64 / device_rate as f64;
    let mut stretch = Stretcher::new(device_channels, device_rate);
    let mut resampler = if src_rate != device_rate {
        Some(LinearResampler::new(src_rate, device_rate, device_channels))
    } else {
        None
    };
    let mut playing = false;
    let mut pending_seek: Option<u64> = None;
    let mut resume_at: Option<u64> = None;
    let mut src_fed: u64 = 0;
    let mut gen = 0u64;
    // 启动/跳转后要丢弃的预热输出帧数（否则开头是拉伸器的高频垃圾）
    let mut skip_out: usize = stretch.warmup_frames();

    loop {
        // ---- 命令 ----
        while let Ok(c) = cmd.try_recv() {
            match c {
                Cmd::Quit => {
                    shared.playing.store(false, Relaxed);
                    return Ok(());
                }
                Cmd::Play => {
                    if let Some(ms) = resume_at.take() {
                        pending_seek = Some(ms);
                    } else if shared.eof.load(Relaxed) {
                        pending_seek = Some(0);
                    }
                    playing = true;
                    shared.playing.store(true, Relaxed);
                }
                Cmd::Pause => {
                    let audible = shared.pos_ms.load(Relaxed);
                    playing = false;
                    shared.playing.store(false, Relaxed);
                    resume_at = Some(audible);
                    gen += 1;
                    shared.gen.store(gen, Relaxed);
                }
                Cmd::Seek(ms) => {
                    pending_seek = Some(ms);
                    resume_at = None;
                    if !playing {
                        // 暂停态跳转：直接定位并更新显示位置
                        shared.pos_ms.store(ms.min(shared.duration_ms.load(Relaxed)), Relaxed);
                    }
                    gen += 1;
                    shared.gen.store(gen, Relaxed);
                }
                Cmd::SetSpeed(s) => {
                    stretch.set_speed(s);
                    shared.speed_milli.store((s * 1000.0) as u64, Relaxed);
                }
                Cmd::Loop { a_ms, b_ms, count } => {
                    shared.loop_a_ms.store(a_ms, Relaxed);
                    shared.loop_b_ms.store(b_ms, Relaxed);
                    shared.loop_left.store(count as u64, Relaxed);
                }
                Cmd::StopAt(ms) => {
                    shared.stop_at_ms.store(ms, Relaxed);
                    shared.reached_stop.store(false, Relaxed);
                }
            }
        }

        // ---- 等待队列被回调清空（跳转生效的同步点）----
        if gen != shared.ack_gen.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // ---- 执行挂起的跳转 ----
        if let Some(ms) = pending_seek.take() {
            let target = ms.min(shared.duration_ms.load(Relaxed));
            dec.seek_ms(target)?;
            stretch.reset();
            skip_out = stretch.warmup_frames();
            if let Some(r) = resampler.as_mut() {
                *r = LinearResampler::new(src_rate, device_rate, device_channels);
            }
            // 位置计数归零：以目标位置为新起点
            src_fed = (target as f64 / 1000.0 * src_rate as f64) as u64;
            shared.src_fed.store(src_fed, Relaxed);
            shared.out_produced.store(0, Relaxed);
            shared.out_consumed.store(0, Relaxed);
            shared.pos_ms.store(target, Relaxed);
            shared.eof.store(false, Relaxed);
            shared.finished.store(false, Relaxed);
        }

        if !playing {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }

        // ---- 解码一块 ----
        let mut src_block: Vec<f32> = Vec::new();
        while src_block.len() < BLOCK_FRAMES * src_ch {
            match dec.next_packet()? {
                Some(buf) => src_block.extend_from_slice(buf),
                None => break,
            }
        }

        let eof = src_block.len() < BLOCK_FRAMES * src_ch;
        if src_block.is_empty() {            // 尾部：把变速器内部缓冲吐出来，然后停
            let tail_frames = stretch.latency_frames().max(BLOCK_FRAMES);
            let mut out = vec![0.0f32; tail_frames * device_channels];
            stretch.flush(&mut out);
            let _ = send_block(&tx, gen, out);
            shared.eof.store(true, Relaxed);
            shared.finished.store(true, Relaxed);
            playing = false;
            shared.playing.store(false, Relaxed);
            resume_at = Some(0);
            continue;
        }

        // ---- 声道数归一化（单声道 → 设备声道数）----
        let frames = src_block.len() / src_ch;
        let mut norm: Vec<f32> = Vec::with_capacity(frames * device_channels);
        if src_ch == device_channels {
            norm = src_block;
        } else if src_ch == 1 {
            for f in 0..frames {
                let v = src_block[f];
                for _ in 0..device_channels {
                    norm.push(v);
                }
            }
        } else {
            for f in 0..frames {
                for c in 0..device_channels {
                    norm.push(src_block[f * src_ch + c.min(src_ch - 1)]);
                }
            }
        }

        // ---- 重采样兜底 ----
        let at_device_rate: Vec<f32> = if let Some(r) = resampler.as_mut() {
            let out_frames = ((frames as f64) / rate_scale).round() as usize;
            let mut y = vec![0.0f32; out_frames.max(1) * device_channels];
            r.process(&norm, &mut y);
            y
        } else {
            norm
        };

        // ---- 变速 ----
        let mut out = stretch.process(&at_device_rate).to_vec();

        // ---- 丢弃预热输出（不算可听内容）----
        if skip_out > 0 {
            let frames_out = out.len() / device_channels;
            let drop_frames = skip_out.min(frames_out);
            skip_out -= drop_frames;
            out.drain(..drop_frames * device_channels);
        }
        if out.is_empty() {
            // 整块都被当作预热丢掉：仍要推进源位置，但不入队
            src_fed += frames as u64;
            shared.src_fed.store(src_fed, Relaxed);
            continue;
        }

        // ---- 入队（阻塞式，尊重命令响应）----
        loop {
            match tx.try_send((gen, out.clone())) {
                Ok(()) => break,
                Err(TrySendError::Full(_)) => {
                    std::thread::sleep(Duration::from_millis(3));
                    if shared.gen.load(Relaxed) != gen {
                        break; // 期间发生了跳转
                    }
                }
                Err(TrySendError::Disconnected(_)) => return Ok(()),
            }
        }

        src_fed += frames as u64;
        shared.src_fed.store(src_fed, Relaxed);
        let produced = shared.out_produced.load(Relaxed) + (out.len() / device_channels) as u64;
        shared.out_produced.store(produced, Relaxed);

        // ---- 更新可听位置 ----
        let consumed = shared.out_consumed.load(Relaxed);
        let pending_out = produced.saturating_sub(consumed);
        let pending_src = (pending_out as f64 * rate_scale * stretch.speed() as f64) as u64;
        let audible_frames = src_fed.saturating_sub(pending_src);
        let pos_ms = (audible_frames as f64 / src_rate as f64 * 1000.0) as u64;
        shared.pos_ms.store(pos_ms, Relaxed);

        // ---- A-B 循环 ----
        let (la, lb, left) = (
            shared.loop_a_ms.load(Relaxed),
            shared.loop_b_ms.load(Relaxed),
            shared.loop_left.load(Relaxed),
        );
        if lb > la && left > 0 && pos_ms >= lb {
            shared.loop_left.store(left - 1, Relaxed);
            pending_seek = Some(la);
            gen += 1;
            shared.gen.store(gen, Relaxed);
            continue;
        }

        // ---- 题间自动暂停 ----
        let stop_at = shared.stop_at_ms.load(Relaxed);
        if stop_at > 0 && pos_ms >= stop_at {
            shared.pos_ms.store(stop_at, Relaxed);
            shared.reached_stop.store(true, Relaxed);
            shared.stop_at_ms.store(0, Relaxed);
            playing = false;
            shared.playing.store(false, Relaxed);
            resume_at = Some(stop_at);
            gen += 1;
            shared.gen.store(gen, Relaxed);
            continue;
        }

        if eof {
            // 下一轮 src_block 会为空，进入上面的收尾分支（把变速器残留输出吐完）
            let _ = eof;
        }
    }
}

fn send_block(tx: &Sender<(u64, Vec<f32>)>, gen: u64, block: Vec<f32>) -> std::result::Result<(), ()> {
    let mut b = block;
    loop {
        match tx.try_send((gen, b)) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(v)) => {
                b = v.1;
                std::thread::sleep(Duration::from_millis(3));
            }
            Err(TrySendError::Disconnected(_)) => return Err(()),
        }
    }
}
