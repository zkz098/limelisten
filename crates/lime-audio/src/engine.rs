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
        Self::new_at(path, 0)
    }

    /// 打开文件并从指定的毫秒位置建立输出流。
    pub fn new_at(path: &Path, start_pos_ms: u64) -> Result<Self> {
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
        let start_pos_ms = start_pos_ms.min(info.duration_ms);
        shared.pos_ms.store(start_pos_ms, Relaxed);

        let (tx, rx) = bounded::<(u64, Vec<f32>)>(QUEUE_BLOCKS);
        // 输出缓冲回收通道：回调消费完的 Vec 由此归还，供传输线程复用（避免每块 malloc/free）
        let (recycle_tx, recycle_rx) = bounded::<Vec<f32>>(QUEUE_BLOCKS + 4);
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(32);

        let stream = build_stream(&device, config, shared.clone(), rx, recycle_tx)?;
        stream
            .play()
            .map_err(|e| Error::Decode(format!("stream play: {e}")))?;

        let sh = shared.clone();
        let path_buf: PathBuf = path.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("lime-transport".into())
            .spawn(move || {
                if let Err(e) = worker_loop(
                    &path_buf,
                    &sh,
                    tx,
                    recycle_rx,
                    cmd_rx,
                    device_rate,
                    device_channels,
                    start_pos_ms,
                ) {
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
    recycle_tx: Sender<Vec<f32>>,
) -> Result<cpal::Stream> {
    match device.default_output_config().map(|c| c.sample_format()) {
        Ok(cpal::SampleFormat::F32) => build_typed::<f32>(device, config, shared, rx, recycle_tx),
        Ok(cpal::SampleFormat::I16) => build_typed::<i16>(device, config, shared, rx, recycle_tx),
        Ok(cpal::SampleFormat::U16) => build_typed::<u16>(device, config, shared, rx, recycle_tx),
        _ => build_typed::<f32>(device, config, shared, rx, recycle_tx),
    }
}

/// 把回调消费完的输出缓冲归还给传输线程复用；通道满或无容量时直接丢弃。
/// 音频回调里只做非阻塞 `try_send`，不会阻塞。
#[inline]
fn recycle(tx: &Sender<Vec<f32>>, v: Vec<f32>) {
    if v.capacity() > 0 {
        let _ = tx.try_send(v);
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    rx: Receiver<(u64, Vec<f32>)>,
    recycle_tx: Sender<Vec<f32>>,
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
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let gen = sh.gen.load(Relaxed);
                if gen != local_gen {
                    // 丢弃全部已排队数据（跳转/暂停/循环时用），并把缓冲返还复用
                    while let Ok((_, v)) = rx.try_recv() {
                        recycle(&recycle_tx, v);
                    }
                    recycle(&recycle_tx, std::mem::take(&mut cur));
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
                                    recycle(&recycle_tx, v);
                                    continue;
                                }
                                recycle(&recycle_tx, std::mem::replace(&mut cur, v));
                                cur_pos = 0;
                            }
                            Err(_) => {
                                for d in data[i..].iter_mut() {
                                    *d = T::from_sample(0.0f32);
                                }
                                // 暂停/播完后队列本来就是空的，这不是欠载；
                                // 否则 UI 上的 underrun 会一直涨，并导致无意义重绘。
                                if sh.playing.load(Relaxed) {
                                    sh.underruns.fetch_add(1, Relaxed);
                                }
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
    Ok(stream)
}

/// 传输线程：解码 → （重采样）→ 变速 → 入队；同时负责跳转/循环/自动暂停。
///
/// 性能：所有中间缓冲（解码块 / 声道归一化 / 重采样 / 输出）都在循环外分配一次并复用；
/// 输出 Vec 由回调通过 `recycle_rx` 归还后再用，稳态下**每块不再有堆分配**。
#[allow(clippy::too_many_arguments)]
fn worker_loop(
    path: &Path,
    shared: &Arc<Shared>,
    tx: Sender<(u64, Vec<f32>)>,
    recycle_rx: Receiver<Vec<f32>>,
    cmd: Receiver<Cmd>,
    device_rate: u32,
    device_channels: usize,
    start_pos_ms: u64,
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
    let mut pending_seek: Option<u64> = if start_pos_ms > 0 {
        Some(start_pos_ms)
    } else {
        None
    };
    let mut resume_at: Option<u64> = None;
    let mut src_fed: u64 = if start_pos_ms > 0 {
        (start_pos_ms as f64 / 1000.0 * src_rate as f64) as u64
    } else {
        0
    };
    let mut gen = 0u64;
    // 启动/跳转后要丢弃的预热输出帧数（否则开头是拉伸器的高频垃圾）
    let mut skip_out: usize = stretch.warmup_frames();

    // 复用的工作缓冲：只在首次到达峰值时分配，稳态下每块不再 malloc/free。
    let mut src_block: Vec<f32> = Vec::new();
    let mut norm: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    let mut free: Vec<Vec<f32>> = Vec::new(); // 回调回收的输出缓冲池
    let mut cmd_buf: Vec<Cmd> = Vec::with_capacity(8);

    loop {
        // 回收回调用完的输出缓冲（池化复用）
        while let Ok(v) = recycle_rx.try_recv() {
            if free.len() < QUEUE_BLOCKS + 4 {
                free.push(v);
            }
        }

        // ---- 命令 ----
        // 暂停/播完时没有数据要产出，阻塞等命令即可；此前的 `sleep(4ms)` 会在
        // 后台每秒唤醒 250 次，低功耗机器上既费电又阻止 CPU 进入深度休眠。
        cmd_buf.clear();
        if playing {
            cmd_buf.extend(cmd.try_iter());
        } else {
            match cmd.recv() {
                Ok(c) => cmd_buf.push(c),
                Err(_) => return Ok(()),
            }
            cmd_buf.extend(cmd.try_iter());
        }
        for c in cmd_buf.drain(..) {
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
                        shared
                            .pos_ms
                            .store(ms.min(shared.duration_ms.load(Relaxed)), Relaxed);
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

        if !playing {
            // 回到循环顶部阻塞等命令；跳转/循环等状态已记录，恢复播放时再生效。
            continue;
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

        // ---- 解码一块（复用 src_block）----
        src_block.clear();
        while src_block.len() < BLOCK_FRAMES * src_ch {
            match dec.next_packet()? {
                Some(buf) => src_block.extend_from_slice(buf),
                None => break,
            }
        }

        let eof = src_block.len() < BLOCK_FRAMES * src_ch;
        if src_block.is_empty() {
            // 尾部：把变速器内部缓冲吐出来，然后停
            let tail_frames = stretch.latency_frames().max(BLOCK_FRAMES);
            let mut out = free.pop().unwrap_or_default();
            out.clear();
            out.resize(tail_frames * device_channels, 0.0);
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
        // 声道数已一致时直接借用解码块，避免多余拷贝。
        let frames = src_block.len() / src_ch;
        norm.clear();
        let normalized: &[f32] = if src_ch == device_channels {
            &src_block
        } else if src_ch == 1 {
            norm.reserve(frames * device_channels);
            for &v in src_block.iter().take(frames) {
                for _ in 0..device_channels {
                    norm.push(v);
                }
            }
            &norm
        } else {
            norm.reserve(frames * device_channels);
            for f in 0..frames {
                for c in 0..device_channels {
                    norm.push(src_block[f * src_ch + c.min(src_ch - 1)]);
                }
            }
            &norm
        };

        // ---- 取一个可复用的输出缓冲 ----
        let mut out = free.pop().unwrap_or_default();

        // ---- 重采样兜底（复用 resampled）----
        if let Some(r) = resampler.as_mut() {
            let out_frames = ((frames as f64) / rate_scale).round() as usize;
            resampled.clear();
            resampled.resize(out_frames.max(1) * device_channels, 0.0);
            r.process(normalized, &mut resampled);
            stretch.process_into(&resampled, &mut out);
        } else {
            stretch.process_into(normalized, &mut out);
        }

        // ---- 丢弃预热输出（不算可听内容）----
        if skip_out > 0 {
            let frames_out = out.len() / device_channels;
            let drop_frames = skip_out.min(frames_out);
            skip_out -= drop_frames;
            out.drain(..drop_frames * device_channels);
        }
        if out.is_empty() {
            // 整块都被当作预热丢掉：仍要推进源位置，但不入队；缓冲放回池里
            src_fed += frames as u64;
            shared.src_fed.store(src_fed, Relaxed);
            free.push(out);
            continue;
        }

        let out_frames = out.len() / device_channels;

        // ---- 入队（移动所有权；满时拿回来重试，不再 clone）----
        let mut payload = Some(out);
        while let Some(block) = payload.take() {
            match tx.try_send((gen, block)) {
                Ok(()) => break,
                Err(TrySendError::Full(v)) => {
                    std::thread::sleep(Duration::from_millis(3));
                    if shared.gen.load(Relaxed) != gen {
                        // 期间发生了跳转：这一块作废，缓冲放回池里复用
                        free.push(v.1);
                        break;
                    }
                    payload = Some(v.1);
                }
                Err(TrySendError::Disconnected(_)) => return Ok(()),
            }
        }

        src_fed += frames as u64;
        shared.src_fed.store(src_fed, Relaxed);
        let produced = shared.out_produced.load(Relaxed) + out_frames as u64;
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

fn send_block(
    tx: &Sender<(u64, Vec<f32>)>,
    gen: u64,
    block: Vec<f32>,
) -> std::result::Result<(), ()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_wav(path: &Path, sr: u32, secs: u32) {
        let samples: Vec<f32> = (0..sr as usize * secs as usize)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
            .collect();
        crate::render::write_wav(path, &samples, 1, sr).expect("write test wav");
    }

    /// 直接驱动 `worker_loop`（不依赖真实声卡），模拟音频回调消费 + 缓冲回收：
    /// 验证队列产出、回收通道、位置推进都能正常工作。
    #[test]
    fn worker_loop_produces_and_recycles() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lime_engine_test_{}.wav", std::process::id()));
        write_test_wav(&path, 48_000, 2);

        let shared = Arc::new(Shared::default());
        shared.speed_milli.store(1000, Relaxed);
        shared.duration_ms.store(2000, Relaxed);

        let (tx, rx) = bounded::<(u64, Vec<f32>)>(8);
        let (recycle_tx, recycle_rx) = bounded::<Vec<f32>>(16);
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(8);
        cmd_tx.send(Cmd::Play).unwrap();

        let sh = shared.clone();
        let p = path.clone();
        let worker = std::thread::spawn(move || {
            worker_loop(&p, &sh, tx, recycle_rx, cmd_rx, 48_000, 1, 0).unwrap();
        });

        // 模拟回调：消费队列帧（计入 out_consumed），并把缓冲回收给 worker 复用
        let mut got = 0usize;
        let mut recycled = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok((_g, v)) => {
                    got += v.len();
                    shared.out_consumed.fetch_add(v.len() as u64, Relaxed);
                    let _ = recycle_tx.try_send(v);
                    recycled += 1;
                }
                Err(_) if shared.finished() => break,
                Err(_) => {}
            }
        }
        // 收尾：把剩余的块也消费掉
        while let Ok((_g, v)) = rx.try_recv() {
            got += v.len();
            shared.out_consumed.fetch_add(v.len() as u64, Relaxed);
            let _ = recycle_tx.try_send(v);
        }
        assert!(got >= 96_000, "只产出 {got} 帧（2 s @48k 应为 96000+）");
        assert!(recycled > 0, "没有块被回收");
        // 消费与产出同步后，可听位置应明显推进
        let pos_before = shared.pos_ms();
        assert!(pos_before > 800, "位置没有推进: {pos_before}");

        // seek 后位置应立即回退到目标附近
        cmd_tx.send(Cmd::Seek(0)).unwrap();
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_secs(2) && shared.pos_ms() > 1000 {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            shared.pos_ms() <= 1000,
            "seek 后位置 {} 未回退",
            shared.pos_ms()
        );

        let _ = cmd_tx.send(Cmd::Quit);
        let _ = worker.join();
        let _ = std::fs::remove_file(&path);
    }

    /// 验证以非零起点启动 worker_loop 时，播放位置直接继承设定值，不会被重置为 0。
    #[test]
    fn worker_loop_starts_at_specified_position() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "lime_engine_start_pos_test_{}.wav",
            std::process::id()
        ));
        write_test_wav(&path, 48_000, 3); // 3 秒音频

        let shared = Arc::new(Shared::default());
        shared.speed_milli.store(1000, Relaxed);
        shared.duration_ms.store(3000, Relaxed);
        shared.pos_ms.store(1000, Relaxed);

        let (tx, rx) = bounded::<(u64, Vec<f32>)>(8);
        let (recycle_tx, recycle_rx) = bounded::<Vec<f32>>(16);
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(8);
        cmd_tx.send(Cmd::Play).unwrap();

        let sh = shared.clone();
        let p = path.clone();
        let worker = std::thread::spawn(move || {
            worker_loop(&p, &sh, tx, recycle_rx, cmd_rx, 48_000, 1, 1000).unwrap();
        });

        // 模拟回调消费
        let mut got = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok((_g, v)) => {
                    got += v.len();
                    shared.out_consumed.fetch_add(v.len() as u64, Relaxed);
                    let _ = recycle_tx.try_send(v);
                }
                Err(_) if shared.finished() => break,
                Err(_) => {}
            }
        }
        assert!(got > 0, "应产出帧");
        let pos = shared.pos_ms();
        // 初始起点为 1000 ms，最终位置必须 >= 1000 ms，绝不应回滚到 0
        assert!(pos >= 1000, "初始位置被重置: {pos}");

        let _ = cmd_tx.send(Cmd::Quit);
        let _ = worker.join();
        let _ = std::fs::remove_file(&path);
    }
}
