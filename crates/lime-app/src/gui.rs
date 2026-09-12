//! GUI 装配：Slint 界面 ↔ 后台分析线程 ↔ 播放引擎 ↔ SQLite 本地库与缓存。
//!
//! 核心设计（P1）：
//! - 库管理与秒开：打开文件或从媒体库选择时，优先查 `library.db`，已分析内容 <100ms 瞬间秒开出章节和字幕；
//! - 后台线程：切分与 Whisper 转写异步进行，完成后自动落库；
//! - 题级微调：支持起止点微调（±0.1s / ±0.5s）、边界试听、光标处拆分、合并下节，修改持久化并锁定（`locked=1`）；
//! - 字幕导出：支持 SRT、LRC、TXT 导出；
//! - 断点续播与统计：自动存取进度，播放/循环写入统计。

use crate::cli::{analyze, analyze_mono, assign_sentences_to_chapters, transcribe, AnalysisCore};
use crate::paths;
use crate::{ChapterRow, MainWindow, MediaRow, ModelBenchmarkRow, WordCell};
use crossbeam_channel::{unbounded, Receiver, Sender};
use lime_audio::Engine;
use lime_core::{
    find_accompanying_audio, find_matching_limed, Chapter, ChapterLevel, ChapterSource, LimedFile,
    LimedMeta, Sentence, Word,
};
use lime_store::{MediaItem, Store};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

enum Msg {
    Status(String),
    AnalyzeDone(Box<AnalysisCore>, Box<Vec<f32>>, u32),
    TranscribeProgress(f32),
    TranscribeDone(Vec<Sentence>),
    Error(String),
    #[allow(dead_code)]
    BenchModelUpdate {
        spec: String,
        bench_status: String,
        is_recommended: bool,
    },
    BenchDone {
        summary: String,
    },
    DownloadProgress {
        spec: String,
        progress: f32,
        status_text: String,
    },
    DownloadDone {
        #[allow(dead_code)]
        spec: String,
        success: bool,
        msg: String,
    },
}

struct GuiState {
    media_id: Option<i64>,
    path: Option<PathBuf>,
    core: Option<AnalysisCore>,
    mono: Vec<f32>,
    mono_rate: u32,
    sentences: Vec<Sentence>,
    /// 当前句的词（点击跳转用）
    current_words: Vec<Word>,
    engine: Option<Engine>,
    speed: f32,
    loop_count: u32,
    auto_pause: bool,
    busy: Arc<AtomicBool>,
    media_list: Vec<MediaItem>,
    last_progress_save: std::time::Instant,
    bench_results: HashMap<String, (String, bool)>,
    downloading_spec: Option<(String, f32, String)>,
    is_downloading: Arc<AtomicBool>,
    is_benchmarking: Arc<AtomicBool>,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            media_id: None,
            path: None,
            core: None,
            mono: Vec::new(),
            mono_rate: 44100,
            sentences: Vec::new(),
            current_words: Vec::new(),
            engine: None,
            speed: 1.0,
            loop_count: 0,
            auto_pause: true,
            busy: Arc::new(AtomicBool::new(false)),
            media_list: Vec::new(),
            last_progress_save: std::time::Instant::now(),
            bench_results: HashMap::new(),
            downloading_spec: None,
            is_downloading: Arc::new(AtomicBool::new(false)),
            is_benchmarking: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl GuiState {
    fn range(&self, idx: usize) -> Option<(u64, u64)> {
        let core = self.core.as_ref()?;
        core.chapters.get(idx).map(|c| (c.start_ms, c.end_ms))
    }
    fn whole_range(&self) -> (u64, u64) {
        (0, self.core.as_ref().map(|c| c.info.duration_ms).unwrap_or(0))
    }
}

fn get_bench_model_rows(
    downloading_spec: Option<(&str, f32, &str)>,
    bench_results: &HashMap<String, (String, bool)>,
) -> Vec<ModelBenchmarkRow> {
    let mut rows = Vec::new();
    for meta in paths::MODEL_SPECS {
        let found = paths::find_model_by_spec(meta.spec);
        let installed = found.is_some();
        let (file_size, actual_filename) = if let Some(ref p) = found {
            let sz = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let fname = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(meta.filename)
                .to_string();
            (format!("{:.1} MB", sz as f64 / 1_048_576.0), fname)
        } else {
            (format!("~{} MB", meta.approx_size_mb), meta.filename.to_string())
        };

        let is_dl = downloading_spec
            .map(|(s, _, _)| s == meta.spec)
            .unwrap_or(false);
        let (dl_prog, dl_stat) = if is_dl {
            let (_, p, text) = downloading_spec.unwrap();
            (p, text.to_string())
        } else if installed {
            (1.0, "已就绪".to_string())
        } else {
            (0.0, "未安装".to_string())
        };

        let (b_stat, is_rec) = if let Some((res_text, rec)) = bench_results.get(meta.spec) {
            (res_text.clone(), *rec)
        } else if !installed {
            ("未安装模型".to_string(), false)
        } else {
            ("未测速".to_string(), false)
        };

        rows.push(ModelBenchmarkRow {
            spec: meta.spec.into(),
            name: meta.name.into(),
            filename: actual_filename.into(),
            desc: meta.desc.into(),
            installed,
            file_size: file_size.into(),
            downloading: is_dl,
            download_progress: dl_prog,
            download_status: dl_stat.into(),
            bench_status: b_stat.into(),
            is_recommended: is_rec,
        });
    }
    rows
}

fn refresh_bench_models_ui(ui: &MainWindow, state: &GuiState) {
    let dl = state
        .downloading_spec
        .as_ref()
        .map(|(s, p, t)| (s.as_str(), *p, t.as_str()));
    let rows = get_bench_model_rows(dl, &state.bench_results);
    ui.set_bench_models(ModelRc::new(VecModel::from(rows)));
}

fn run_model_download(tx: Sender<Msg>, spec: String) {
    let Some(meta) = paths::get_spec_meta(&spec) else {
        let _ = tx.send(Msg::DownloadDone {
            spec,
            success: false,
            msg: "未知模型规格".into(),
        });
        return;
    };

    let models_dir = paths::models_dir();
    let dest_path = models_dir.join(meta.filename);
    let temp_path = models_dir.join(format!("{}.downloading", meta.filename));
    let _ = std::fs::remove_file(&temp_path);

    let urls = [meta.primary_url, meta.fallback_url];
    let mut success = false;
    let mut last_err = String::new();

    for url in urls {
        let _ = tx.send(Msg::DownloadProgress {
            spec: spec.clone(),
            progress: 0.02,
            status_text: format!("正在连接 ({:.1}MB)...", meta.size_bytes as f64 / 1_048_576.0),
        });

        let mut cmd = std::process::Command::new("curl.exe");
        cmd.arg("-L")
            .arg("-k")
            .arg("--fail")
            .arg("--retry")
            .arg("2")
            .arg("-o")
            .arg(&temp_path)
            .arg(url);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let start = std::time::Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("启动 curl 失败: {e}");
                continue;
            }
        };

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        let len = std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);
                        if len >= 1024 * 1024 {
                            let _ = std::fs::rename(&temp_path, &dest_path);
                            success = true;
                        } else {
                            last_err = "下载文件异常 (体积过小)".into();
                        }
                    } else {
                        last_err = format!("curl 退出代码 {:?}", status.code());
                    }
                    break;
                }
                Ok(None) => {
                    let cur = std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);
                    let prog = (cur as f64 / meta.size_bytes as f64).clamp(0.02, 0.99) as f32;
                    let elapsed = start.elapsed().as_secs_f64().max(0.1);
                    let speed = (cur as f64 / 1_048_576.0) / elapsed;
                    let text = format!(
                        "{:.0}% ({:.1}MB/{:.1}MB, {:.1}MB/s)",
                        prog * 100.0,
                        cur as f64 / 1_048_576.0,
                        meta.size_bytes as f64 / 1_048_576.0,
                        speed
                    );
                    let _ = tx.send(Msg::DownloadProgress {
                        spec: spec.clone(),
                        progress: prog,
                        status_text: text,
                    });
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => {
                    last_err = e.to_string();
                    break;
                }
            }
        }

        if success {
            break;
        } else {
            let _ = std::fs::remove_file(&temp_path);
        }
    }

    let _ = tx.send(Msg::DownloadDone {
        spec,
        success,
        msg: if success {
            format!("✅ {} 模型下载成功并已就绪！", meta.name)
        } else {
            format!("❌ {} 模型下载失败: {}", meta.name, last_err)
        },
    });
}

#[cfg(feature = "whisper")]
fn run_gui_benchmark(tx: Sender<Msg>, audio_path: Option<PathBuf>) {
    let _ = tx.send(Msg::Status("正在准备 15 秒测速切片...".into()));
    let sample_str = audio_path.as_deref().and_then(|p| p.to_str());
    let (test_wav, audio_dur_s) = match crate::cli::prepare_benchmark_wav(sample_str) {
        Ok(res) => res,
        Err(e) => {
            let _ = tx.send(Msg::BenchDone {
                summary: format!("测速准备失败: {e}"),
            });
            return;
        }
    };

    let exe = match paths::find_whisper_exe() {
        Some(e) => e,
        None => {
            let _ = tx.send(Msg::BenchDone {
                summary: "未找到 whisper-cli.exe，无法执行模型硬件测速".into(),
            });
            return;
        }
    };

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut best_model: Option<(&'static str, f64, bool)> = None;

    for meta in paths::MODEL_SPECS {
        let model_path = match paths::find_model_by_spec(meta.spec) {
            Some(p) => p,
            None => {
                let _ = tx.send(Msg::BenchModelUpdate {
                    spec: meta.spec.into(),
                    bench_status: "未安装模型".into(),
                    is_recommended: false,
                });
                continue;
            }
        };

        let _ = tx.send(Msg::BenchModelUpdate {
            spec: meta.spec.into(),
            bench_status: "测速中...".into(),
            is_recommended: false,
        });

        let out_base = paths::cache_dir().join(format!("gui_bench_{}", meta.spec));
        let start = std::time::Instant::now();

        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("-m")
            .arg(&model_path)
            .arg("-f")
            .arg(&test_wav)
            .arg("-l")
            .arg("en")
            .arg("-t")
            .arg(threads.to_string())
            .arg("-oj")
            .arg("-ml")
            .arg("1")
            .arg("-sow")
            .arg("-pp")
            .arg("-of")
            .arg(&out_base)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::BenchModelUpdate {
                    spec: meta.spec.into(),
                    bench_status: format!("❌ 启动失败: {e}"),
                    is_recommended: false,
                });
                continue;
            }
        };

        let mut timed_out = false;
        let mut child_err = None;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        child_err = Some(format!("exit: {status}"));
                    }
                    break;
                }
                Ok(None) => {
                    if start.elapsed().as_secs_f64() > audio_dur_s {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => {
                    child_err = Some(e.to_string());
                    break;
                }
            }
        }

        if timed_out {
            let _ = tx.send(Msg::BenchModelUpdate {
                spec: meta.spec.into(),
                bench_status: "❌ 超时熔断(RTF>1.0)".into(),
                is_recommended: false,
            });
        } else if let Some(e) = child_err {
            let _ = tx.send(Msg::BenchModelUpdate {
                spec: meta.spec.into(),
                bench_status: format!("❌ 出错: {e}"),
                is_recommended: false,
            });
        } else {
            let elapsed = start.elapsed().as_secs_f64();
            let speedup = audio_dur_s / elapsed;
            let is_rec = speedup >= 3.0;
            let status = if speedup >= 10.0 {
                format!("{speedup:.1}x ({elapsed:.2}s) ★★★★★ 极速")
            } else if is_rec {
                format!("{speedup:.1}x ({elapsed:.2}s) ✅ 推荐")
            } else {
                format!("{speedup:.1}x ({elapsed:.2}s) ⚠️ 较慢")
            };
            let _ = tx.send(Msg::BenchModelUpdate {
                spec: meta.spec.into(),
                bench_status: status,
                is_recommended: is_rec,
            });

            if is_rec {
                if best_model.is_none() || meta.spec == "turbo" {
                    best_model = Some((meta.name, speedup, true));
                }
            } else if best_model.is_none() {
                best_model = Some((meta.name, speedup, false));
            }
        }
    }

    let summary = if let Some((name, speedup, true)) = best_model {
        format!("✅ 最佳推荐模型: {name} (倍速 {speedup:.1}x) · 性能符合 3:1 标准，推荐作为主转写模型")
    } else if let Some((_name, speedup, false)) = best_model {
        format!("⚠️ 已测模型最高倍速仅 {speedup:.1}x (低于 3:1 标准)。建议下载 Tiny/Base 规格，或使用 Slim 纯听版直接消费 .limed 缓存。")
    } else {
        "未完成有效测速。请检查模型或点击「下载」轻量规格模型后重试。".to_string()
    };

    let _ = tx.send(Msg::BenchDone { summary });
}

#[cfg(not(feature = "whisper"))]
fn run_gui_benchmark(tx: Sender<Msg>, _audio_path: Option<PathBuf>) {
    let _ = tx.send(Msg::BenchDone {
        summary: "Slim 纯听版已裁剪 Whisper 引擎，无需测速，可直接打开 .limed 缓存秒开音频。".into(),
    });
}

pub fn run() -> anyhow::Result<()> {
    let ui = MainWindow::new()?;
    let db_path = paths::data_dir().join("library.db");
    let store = Rc::new(RefCell::new(Store::open(&db_path)?));
    let state = Rc::new(RefCell::new(GuiState {
        speed: 1.0,
        auto_pause: true,
        ..Default::default()
    }));
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = unbounded();
    let last_sentence = Rc::new(Cell::new(usize::MAX));

    let is_slim = cfg!(not(feature = "whisper"));
    ui.set_is_slim(is_slim);
    if is_slim {
        ui.set_app_title("limelisten — 听力播放器 (Slim 纯听版)".into());
        ui.set_status("Slim 纯听版已就绪 · 配合同名 .limed 缓存直接加载字幕".into());
    }

    // 启动时刷新媒体库列表与模型列表
    refresh_media_list(&ui, &store.borrow(), &mut state.borrow_mut());
    refresh_bench_models_ui(&ui, &state.borrow());

    // ---------------- 打开单个音频文件 / .limed 缓存 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        let tx = tx.clone();
        let last_sentence = Rc::clone(&last_sentence);
        ui.on_open_file(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "音频 / LIMED缓存",
                    &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "limed"],
                )
                .pick_file()
            else {
                return;
            };
            load_or_analyze_media(path, &ui, &state, &store, &tx, &last_sentence);
        });
    }

    // ---------------- 扫描文件夹批量入库 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        let tx = tx.clone();
        let last_sentence = Rc::clone(&last_sentence);
        ui.on_open_folder(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };

            let mut found = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
                        if matches!(ext.as_str(), "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus") {
                            found.push(p);
                        }
                    }
                }
            }
            if found.is_empty() {
                ui.set_status("该文件夹内未找到支持的音频文件".into());
                return;
            }
            found.sort();

            for p in &found {
                let size = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0);
                let mtime = std::fs::metadata(p)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let dur = lime_audio::probe(p).map(|i| i.duration_ms as i64).unwrap_or(0);
                let _ = store.borrow().upsert_media(
                    &p.to_string_lossy(),
                    size,
                    mtime,
                    dur,
                    44100,
                    2,
                );
            }

            refresh_media_list(&ui, &store.borrow(), &mut state.borrow_mut());
            ui.set_sidebar_tab(1); // 切到媒体库 tab
            ui.set_status(format!("已扫描导入 {} 个音频文件", found.len()).into());

            if state.borrow().path.is_none() {
                if let Some(first) = found.into_iter().next() {
                    load_or_analyze_media(first, &ui, &state, &store, &tx, &last_sentence);
                }
            }
        });
    }

    // ---------------- 从媒体库列表选择文件 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        let tx = tx.clone();
        let last_sentence = Rc::clone(&last_sentence);
        ui.on_select_media(move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx < 0 { return };
            let item = {
                let st = state.borrow();
                st.media_list.get(idx as usize).cloned()
            };
            let Some(item) = item else { return };
            ui.set_current_media_idx(idx);
            load_or_analyze_media(PathBuf::from(item.path), &ui, &state, &store, &tx, &last_sentence);
        });
    }

    // ---------------- ① 手动触发切分 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        let tx = tx.clone();
        ui.on_analyze(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(path) = state.borrow().path.clone() else {
                ui.set_status("请先打开一个音频文件".into());
                return;
            };
            spawn_analyze(path, ui_weak.clone(), tx.clone(), state.clone(), store.clone());
        });
    }

    // ---------------- ② 转写字幕 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        ui.on_transcribe(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if cfg!(not(feature = "whisper")) {
                ui.set_status("当前为 Slim 纯听版（未包含 Whisper），请配合同名 .limed 缓存使用".into());
                return;
            }
            let payload = {
                let st = state.borrow();
                match st.core.as_ref() {
                    None => None,
                    Some(c) => Some((
                        AnalysisCore {
                            path: c.path.clone(),
                            info: c.info,
                            structure: c.structure.clone(),
                            chimes: c.chimes.clone(),
                            chapters: c.chapters.clone(),
                        },
                        st.busy.clone(),
                    )),
                }
            };
            let Some((core, busy)) = payload else {
                ui.set_status("请先切分音频（转写需要语音岛和地标来进行吸附）".into());
                return;
            };
            if busy.swap(true, Ordering::SeqCst) {
                ui.set_status("已有任务在跑…".into());
                return;
            }
            ui.set_transcribe_progress(0.0);
            let tx2 = tx.clone();
            std::thread::spawn(move || {
                match analyze_mono(&core.path) {
                    Ok((mono, _)) => {
                        let r = transcribe(&core, &mono, |m, p| {
                            let _ = tx2.send(Msg::Status(m.to_string()));
                            let _ = tx2.send(Msg::TranscribeProgress(p));
                        });
                        match r {
                            Ok(t) => {
                                let mut sents = t.sentences;
                                assign_sentences_to_chapters(&mut sents, &core.chapters);
                                let _ = tx2.send(Msg::TranscribeDone(sents));
                            }
                            Err(e) => {
                                let _ = tx2.send(Msg::Error(format!("转写失败：{e}")));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx2.send(Msg::Error(format!("解码失败：{e}")));
                    }
                }
                busy.store(false, Ordering::SeqCst);
            });
        });
    }

    // ---------------- ③ 保存为 .limed 预切分转译缓存 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_limed(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (path, info, chapters, sentences) = {
                let st = state.borrow();
                let Some(path) = st.path.clone() else {
                    ui.set_status("请先打开一个音频文件".into());
                    return;
                };
                let Some(core) = st.core.as_ref() else {
                    ui.set_status("暂无切分或字幕数据可保存".into());
                    return;
                };
                (path, core.info, core.chapters.clone(), st.sentences.clone())
            };
            if chapters.is_empty() && sentences.is_empty() {
                ui.set_status("当前尚无章节或字幕内容可保存".into());
                return;
            }
            let meta = LimedMeta {
                audio_filename: path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                duration_ms: info.duration_ms,
                sample_rate: info.sample_rate,
                channels: info.channels,
                file_size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                version: 1,
                generator: format!("limelisten {}", env!("CARGO_PKG_VERSION")),
            };
            let limed = LimedFile::new(meta, chapters, sentences);
            let target = path.with_extension("limed");
            match limed.save(&target) {
                Ok(()) => {
                    let size_kb = std::fs::metadata(&target)
                        .map(|m| m.len() as f64 / 1024.0)
                        .unwrap_or(0.0);
                    ui.set_status(
                        format!(
                            "✅ 成功保存 .limed 缓存（{:.1} KB）：{}",
                            size_kb,
                            target.file_name().unwrap_or_default().to_string_lossy()
                        )
                        .into(),
                    );
                }
                Err(e) => {
                    ui.set_status(format!("保存 .limed 失败：{e}").into());
                }
            }
        });
    }

    // ---------------- 播放 / 暂停（懒建引擎） ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_play_pause(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let need_engine = state.borrow().engine.is_none();
            if need_engine {
                let Some(path) = state.borrow().path.clone() else {
                    ui.set_status("请先打开一个音频文件".into());
                    return;
                };
                match Engine::new(&path) {
                    Ok(eng) => {
                        state.borrow_mut().engine = Some(eng);
                        ui.set_status("播放引擎已就绪".into());
                    }
                    Err(e) => {
                        ui.set_status(format!("建播放流失败：{e}").into());
                        return;
                    }
                }
            }
            let st = state.borrow();
            if let Some(eng) = st.engine.as_ref() {
                let (a, b) = st.range(ui.get_current().max(0) as usize).unwrap_or_else(|| st.whole_range());
                eng.set_speed(st.speed);
                eng.set_loop(a, b, st.loop_count);
                eng.set_stop_at(if st.auto_pause { b } else { 0 });
                eng.toggle();
                ui.set_playing(eng.shared().playing());
            }
        });
    }

    // ---------------- 章节跳转 ----------------
    for (dir, cb_name) in [(-1i32, "prev"), (1, "next")] {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let cb = move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let n = ui.get_chapters().row_count() as i32;
            if n == 0 {
                return;
            }
            let cur = ui.get_current();
            let next = if dir > 0 { (cur + 1).min(n - 1) } else { (cur - 1).max(0) };
            ui.set_current(next);
            let st = state.borrow();
            if let Some((a, b)) = st.range(next as usize) {
                apply_range(&st, &ui, a, b);
            }
        };
        if cb_name == "prev" {
            ui.on_prev_chapter(cb);
        } else {
            ui.on_next_chapter(cb);
        }
    }

    // ---------------- 选中章节 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_select_chapter(move |idx| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx < 0 {
                return;
            }
            ui.set_current(idx);
            let st = state.borrow();
            if let Some((a, b)) = st.range(idx as usize) {
                apply_range(&st, &ui, a, b);
            }
        });
    }

    // ---------------- 拖动进度 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_seek_to(move |v| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            if let Some(eng) = st.engine.as_ref() {
                eng.seek_ms(v.max(0.0) as u64);
            }
            ui.set_position(v);
        });
    }

    // ---------------- 速度 ----------------
    for (delta, name) in [(0.25f32, "up"), (-0.25, "down")] {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let cb = move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.speed = (st.speed + delta).clamp(0.5, 2.0);
            let s = st.speed;
            if let Some(eng) = st.engine.as_ref() {
                eng.set_speed(s);
            }
            ui.set_info(format!("{s:.2}x").into());
        };
        if name == "up" {
            ui.on_speed_up(cb);
        } else {
            ui.on_speed_down(cb);
        }
    }

    // ---------------- 循环次数 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_loop_cycle(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.loop_count = if st.loop_count >= 3 { 0 } else { st.loop_count + 1 };
            let n = st.loop_count;
            if let Some((a, b)) = st.range(ui.get_current().max(0) as usize) {
                if let Some(eng) = st.engine.as_ref() {
                    eng.set_loop(a, b, n);
                }
            }
            ui.set_loop_count(n as i32);
        });
    }

    // ---------------- 题间自动暂停开关 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_toggle_auto_pause(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.borrow_mut();
            st.auto_pause = !st.auto_pause;
            let ap = st.auto_pause;
            if let Some(eng) = st.engine.as_ref() {
                let stop = if ap {
                    st.range(ui.get_current().max(0) as usize).map(|(_, b)| b).unwrap_or(0)
                } else {
                    0
                };
                eng.set_stop_at(stop);
            }
            ui.set_auto_pause(ap);
        });
    }

    // ---------------- 点击字幕词跳转 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_click_word(move |idx| {
            let Some(_ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            if let Some(w) = st.current_words.get(idx.max(0) as usize) {
                if let Some(eng) = st.engine.as_ref() {
                    eng.seek_ms(w.start_ms);
                }
            }
        });
    }

    // ---------------- 题级微调命令 (P1-3) ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        ui.on_adjust_chapter(move |delta_s, is_start| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let cur_idx = ui.get_current();
            if cur_idx < 0 {
                return;
            }
            let delta_ms = (delta_s as f64 * 1000.0).round() as i64;
            let mut st = state.borrow_mut();
            let mid = st.media_id;
            let loop_count = st.loop_count;
            let auto_pause = st.auto_pause;

            let (ord, s, e, title, rows) = {
                let Some(core) = st.core.as_mut() else { return };
                let Some(ch) = core.chapters.get_mut(cur_idx as usize) else { return };

                if is_start {
                    let new_start = (ch.start_ms as i64 + delta_ms)
                        .clamp(0, ch.end_ms.saturating_sub(100) as i64) as u64;
                    ch.start_ms = new_start;
                } else {
                    let max_d = core.info.duration_ms;
                    let new_end = (ch.end_ms as i64 + delta_ms)
                        .clamp((ch.start_ms + 100) as i64, max_d as i64) as u64;
                    ch.end_ms = new_end;
                }
                ch.source = ChapterSource::Manual;
                ch.locked = true;
                (ch.ordinal, ch.start_ms, ch.end_ms, ch.title.clone(), chapter_rows(core))
            };

            if let Some(mid) = mid {
                let _ = store.borrow().update_chapter_range(mid, ord, s, e);
            }
            ui.set_chapters(ModelRc::new(VecModel::from(rows)));
            if let Some(eng) = st.engine.as_ref() {
                eng.set_loop(s, e, loop_count);
                eng.set_stop_at(if auto_pause { e } else { 0 });
            }
            ui.set_status(format!("已手动微调 [{}]：{} – {} (已锁定)", title, crate::cli::fmt_ms(s), crate::cli::fmt_ms(e)).into());
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        ui.on_split_current_chapter(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let cur_idx = ui.get_current();
            if cur_idx < 0 {
                return;
            }
            let pos = ui.get_position() as u64;

            let mut st = state.borrow_mut();
            let mid = st.media_id;
            let ord = {
                let Some(core) = st.core.as_ref() else { return };
                let Some(ch) = core.chapters.get(cur_idx as usize) else { return };
                ch.ordinal
            };

            if let Some(mid) = mid {
                let _ = store.borrow().split_chapter(mid, ord, pos);
                if let Ok(updated) = store.borrow().load_chapters(mid) {
                    if let Some(core) = st.core.as_mut() {
                        core.chapters = updated;
                        let rows = chapter_rows(core);
                        ui.set_chapters(ModelRc::new(VecModel::from(rows)));
                        ui.set_status(format!("已在 {} 处拆分章节", crate::cli::fmt_ms(pos)).into());
                    }
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        ui.on_merge_next_chapter(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let cur_idx = ui.get_current();
            if cur_idx < 0 {
                return;
            }

            let mut st = state.borrow_mut();
            let mid = st.media_id;
            let ord = {
                let Some(core) = st.core.as_ref() else { return };
                let Some(ch) = core.chapters.get(cur_idx as usize) else { return };
                ch.ordinal
            };

            if let Some(mid) = mid {
                let _ = store.borrow().merge_next_chapter(mid, ord);
                if let Ok(updated) = store.borrow().load_chapters(mid) {
                    if let Some(core) = st.core.as_mut() {
                        core.chapters = updated;
                        let rows = chapter_rows(core);
                        ui.set_chapters(ModelRc::new(VecModel::from(rows)));
                        ui.set_status(format!("已将第 {ord} 题与下一题合并").into());
                    }
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_preview_boundary(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let cur_idx = ui.get_current();
            if cur_idx < 0 {
                return;
            }
            let st = state.borrow();
            let Some(range) = st.range(cur_idx as usize) else { return };
            let end = range.1;
            let start = end.saturating_sub(2000).max(range.0);
            if let Some(eng) = st.engine.as_ref() {
                eng.seek_ms(start);
                eng.set_stop_at(end);
                eng.play();
                ui.set_playing(true);
                ui.set_position(start as f32);
                ui.set_status(format!("试听边界：{} – {}", crate::cli::fmt_ms(start), crate::cli::fmt_ms(end)).into());
            }
        });
    }

    // ---------------- 字幕导出 (P1-4: SRT / LRC / TXT) ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_export_subtitles(move |fmt| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            if st.sentences.is_empty() {
                ui.set_status("当前音频尚无字幕，请先完成「② 转写字幕」".into());
                return;
            }
            let base_name = st
                .path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "subtitles".into());

            let ext = fmt.as_str();
            let filter_name = match ext {
                "lrc" => "LRC 歌词 (*.lrc)",
                "txt" => "TXT 纯文本 (*.txt)",
                _ => "SRT 字幕 (*.srt)",
            };

            let save_dialog = rfd::FileDialog::new()
                .set_file_name(&format!("{base_name}.{ext}"))
                .add_filter(filter_name, &[ext]);

            let Some(save_path) = save_dialog.save_file() else {
                return;
            };

            let content = match ext {
                "lrc" => {
                    let mut out = String::new();
                    for s in &st.sentences {
                        out.push_str(&format!("{}{}\n", fmt_lrc_time(s.start_ms), s.text.trim()));
                    }
                    out
                }
                "txt" => {
                    let mut out = String::new();
                    for s in &st.sentences {
                        out.push_str(&format!("{}\n", s.text.trim()));
                    }
                    out
                }
                _ => {
                    let mut out = String::new();
                    for (i, s) in st.sentences.iter().enumerate() {
                        out.push_str(&format!(
                            "{}\n{} --> {}\n{}\n\n",
                            i + 1,
                            fmt_srt_time(s.start_ms),
                            fmt_srt_time(s.end_ms),
                            s.text.trim()
                        ));
                    }
                    out
                }
            };

            match std::fs::write(&save_path, content.as_bytes()) {
                Ok(_) => ui.set_status(format!("成功导出字幕：{}", save_path.file_name().unwrap_or_default().to_string_lossy()).into()),
                Err(e) => ui.set_status(format!("导出失败：{e}").into()),
            }
        });
    }

    // ---------------- 测速与模型管理模态框回调 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_bench_modal(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            refresh_bench_models_ui(&ui, &st);
            ui.set_show_bench_modal(true);
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_close_bench_modal(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_show_bench_modal(false);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        ui.on_start_bench(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.borrow();
            if st.is_benchmarking.swap(true, Ordering::SeqCst) {
                return;
            }
            ui.set_is_benchmarking(true);
            ui.set_bench_summary("正在测速中...（单模型上限 15 秒，超时自动熔断）".into());
            let audio_path = st.path.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                run_gui_benchmark(tx, audio_path);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        ui.on_download_model(move |spec_slint| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let spec = spec_slint.to_string();
            let st = state.borrow();
            if st.is_downloading.swap(true, Ordering::SeqCst) {
                ui.set_status("当前已有模型正在下载中，请等待其完成".into());
                return;
            }
            let tx = tx.clone();
            std::thread::spawn(move || {
                run_model_download(tx, spec);
            });
        });
    }

    // ---------------- 50ms 定时器 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        let last_sentence = Rc::clone(&last_sentence);
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
            let Some(ui) = ui_weak.upgrade() else { return };

            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Status(s) => ui.set_status(s.into()),
                    Msg::TranscribeProgress(p) => ui.set_transcribe_progress(p),
                    Msg::Error(e) => {
                        ui.set_transcribe_progress(-1.0);
                        ui.set_status(e.into());
                    }
                    Msg::BenchModelUpdate {
                        spec,
                        bench_status,
                        is_recommended,
                    } => {
                        let mut st = state.borrow_mut();
                        st.bench_results.insert(spec, (bench_status, is_recommended));
                        refresh_bench_models_ui(&ui, &st);
                    }
                    Msg::BenchDone { summary } => {
                        ui.set_is_benchmarking(false);
                        ui.set_bench_summary(summary.into());
                        let st = state.borrow();
                        st.is_benchmarking.store(false, Ordering::Relaxed);
                        refresh_bench_models_ui(&ui, &st);
                    }
                    Msg::DownloadProgress {
                        spec,
                        progress,
                        status_text,
                    } => {
                        let mut st = state.borrow_mut();
                        st.downloading_spec = Some((spec, progress, status_text));
                        refresh_bench_models_ui(&ui, &st);
                    }
                    Msg::DownloadDone {
                        spec: _,
                        success,
                        msg,
                    } => {
                        let mut st = state.borrow_mut();
                        st.downloading_spec = None;
                        st.is_downloading.store(false, Ordering::Relaxed);
                        refresh_bench_models_ui(&ui, &st);
                        ui.set_status(msg.into());
                        if success {
                            ui.set_bench_summary("模型下载已完成！可点击「开始测速」测试该模型性能。".into());
                        }
                    }
                    Msg::TranscribeDone(sentences) => {
                        ui.set_transcribe_progress(-1.0);
                        ui.set_has_subtitles(true);
                        ui.set_status(format!("② 转写完成：{} 句字幕", sentences.len()).into());
                        last_sentence.set(usize::MAX);
                        let mut st = state.borrow_mut();
                        if let Some(mid) = st.media_id {
                            let _ = store.borrow().save_sentences(mid, &sentences);
                            let _ = store.borrow().save_analysis(mid, "whisper-turbo", "", "");
                            refresh_media_list(&ui, &store.borrow(), &mut st);
                        }
                        st.sentences = sentences;
                    }
                    Msg::AnalyzeDone(core, mono, rate) => {
                        let rows = chapter_rows(&core);
                        let n_mat = core
                            .chapters
                            .iter()
                            .filter(|c| c.level == ChapterLevel::Material)
                            .count();
                        let strong =
                            core.chimes.iter().filter(|c| c.confidence >= 0.35).count();
                        ui.set_chapters(ModelRc::new(VecModel::from(rows)));
                        ui.set_has_chapters(!core.chapters.is_empty());
                        ui.set_duration(core.info.duration_ms as f32);
                        ui.set_chime_info(
                            format!(
                                "叮咚 {} 处（强 {}）· 材料 {} / 题 {}",
                                core.chimes.len(),
                                strong,
                                n_mat,
                                core.chapters.len() - n_mat
                            )
                            .into(),
                        );
                        if cfg!(not(feature = "whisper")) {
                            ui.set_status(
                                format!(
                                    "① 切分完成：{} 材料 / {} 题（Slim 纯听版未内置 Whisper，字幕请配合同名 .limed 使用）",
                                    n_mat,
                                    core.chapters.len() - n_mat
                                )
                                .into(),
                            );
                        } else {
                            ui.set_status(
                                format!(
                                    "① 切分完成：{} 材料 / {} 题 · 语音岛 {} · 间隙 {}（数字静音 {}）",
                                    n_mat,
                                    core.chapters.len() - n_mat,
                                    core.structure.speech.len(),
                                    core.structure.gaps.len(),
                                    if core.structure.has_digital_silence { "有" } else { "无" }
                                )
                                .into(),
                            );
                        }
                        let mut st = state.borrow_mut();
                        if let Some(mid) = st.media_id {
                            let _ = store.borrow().replace_chapters(mid, &core.chapters);
                            refresh_media_list(&ui, &store.borrow(), &mut st);
                        }
                        st.core = Some(*core);
                        st.mono = *mono;
                        st.mono_rate = rate;
                        let idx = ui.get_current().max(0) as usize;
                        if let (Some(eng), Some((a, b))) = (st.engine.as_ref(), st.range(idx)) {
                            eng.set_loop(a, b, st.loop_count);
                            eng.set_stop_at(if st.auto_pause { b } else { 0 });
                        }
                    }
                }
            }

            // 引擎进度
            let mut pos = ui.get_position() as u64;
            {
                let st = state.borrow();
                if let Some(eng) = st.engine.as_ref() {
                    let sh = eng.shared();
                    pos = sh.pos_ms();
                    ui.set_position(pos as f32);
                    ui.set_playing(sh.playing());
                    ui.set_underruns(format!("underrun {}", sh.underruns()).into());
                    if sh.take_reached_stop() {
                        let n = ui.get_chapters().row_count() as i32;
                        let cur = ui.get_current();
                        let next = (cur + 1).min(n - 1);
                        if next != cur && st.auto_pause {
                            ui.set_current(next);
                            if let Some((a, b)) = st.range(next as usize) {
                                eng.set_loop(a, b, st.loop_count);
                                eng.set_stop_at(b);
                            }
                            ui.set_status("已暂停到下一题".into());
                        }
                        if let (Some(mid), true) = (st.media_id, cur >= 0) {
                            let _ = store.borrow().bump_stat(mid, (cur + 1) as i64, false);
                        }
                    }
                }
            }

            // 时间显示
            let dur = ui.get_duration() as u64;
            ui.set_time_display(format!("{} / {}", crate::cli::fmt_ms(pos), crate::cli::fmt_ms(dur)).into());

            // 定期保存断点续播进度 (每 1.5 秒保存一次)
            {
                let mut st = state.borrow_mut();
                if let Some(mid) = st.media_id {
                    if st.last_progress_save.elapsed() > std::time::Duration::from_millis(1500) {
                        st.last_progress_save = std::time::Instant::now();
                        let cur_ord = (ui.get_current().max(0) + 1) as u32;
                        let _ = store.borrow().save_progress(mid, pos, cur_ord);
                    }
                }
            }

            // 字幕
            if ui.get_has_subtitles() {
                let mut st = state.borrow_mut();
                update_subtitle(&ui, &mut st, pos, &last_sentence);
            }
        });
        std::mem::forget(timer);
    }

    ui.run()?;
    Ok(())
}

fn apply_range(st: &GuiState, ui: &MainWindow, a: u64, b: u64) {
    if let Some(eng) = st.engine.as_ref() {
        eng.set_loop(a, b, st.loop_count);
        eng.set_stop_at(if st.auto_pause { b } else { 0 });
        eng.seek_ms(a);
    }
    ui.set_position(a as f32);
}

fn load_or_analyze_media(
    path: PathBuf,
    ui: &MainWindow,
    state: &Rc<RefCell<GuiState>>,
    store: &Rc<RefCell<Store>>,
    tx: &Sender<Msg>,
    last_sentence: &Rc<Cell<usize>>,
) {
    let path = if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("limed"))
        .unwrap_or(false)
    {
        match find_accompanying_audio(&path) {
            Some(audio) => audio,
            None => {
                ui.set_status("未在同目录下找到对应的音频文件（如 .mp3 / .wav / .m4a）".into());
                return;
            }
        }
    } else {
        path
    };

    let filename = file_label(&path);
    ui.set_current_track_title(filename.clone().into());
    last_sentence.set(usize::MAX);
    ui.set_current(-1);
    ui.set_lines(ModelRc::new(VecModel::from(Vec::<ModelRc<WordCell>>::new())));
    ui.set_active_word(-1);
    ui.set_has_subtitles(false);
    ui.set_has_chapters(false);
    ui.set_next_line("".into());
    ui.set_chime_info("".into());
    ui.set_playing(false);
    ui.set_position(0.0);

    let Ok(info) = lime_audio::probe(&path) else {
        ui.set_status(format!("无法解析音频：{}", path.display()).into());
        return;
    };

    let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
    let mtime = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let media_id = {
        let st_ref = store.borrow();
        st_ref
            .upsert_media(
                &path.to_string_lossy(),
                size,
                mtime,
                info.duration_ms as i64,
                info.sample_rate as i64,
                info.channels as i64,
            )
            .ok()
    };

    {
        let mut st = state.borrow_mut();
        st.path = Some(path.clone());
        st.media_id = media_id;
        st.core = None;
        st.sentences.clear();
        st.current_words.clear();
        st.engine = None;
    }

    // ① 优先检查是否有符合同名 .limed 预切分转译缓存
    let mut cache_hit = false;
    if let Some(limed_path) = find_matching_limed(&path, info.duration_ms, Some(size as u64)) {
        if let Ok(limed) = LimedFile::load(&limed_path) {
            cache_hit = true;
            let chapters = limed.chapters;
            let sents = limed.sentences;
            if let Some(mid) = media_id {
                let st_ref = store.borrow();
                let _ = st_ref.replace_chapters(mid, &chapters);
                let _ = st_ref.save_sentences(mid, &sents);
            }
            let rows = chapter_rows_from_list(&chapters);
            ui.set_chapters(ModelRc::new(VecModel::from(rows)));
            ui.set_has_chapters(!chapters.is_empty());
            ui.set_duration(info.duration_ms as f32);

            let n_mat = chapters
                .iter()
                .filter(|c| c.level == ChapterLevel::Material)
                .count();
            ui.set_chime_info(
                format!("来自 .limed 缓存 · 材料 {} / 题 {}", n_mat, chapters.len() - n_mat).into(),
            );

            if !sents.is_empty() {
                ui.set_has_subtitles(true);
                ui.set_status(
                    format!(
                        "⚡ 成功秒开匹配的 .limed 缓存：{} 章节 · {} 句字幕",
                        chapters.len(),
                        sents.len()
                    )
                    .into(),
                );
            } else {
                ui.set_status(format!("⚡ 成功秒开匹配的 .limed 缓存：{} 章节", chapters.len()).into());
            }

            let (saved_pos, saved_ord) = if let Some(mid) = media_id {
                store.borrow().get_progress(mid).unwrap_or(None).unwrap_or((0, 0))
            } else {
                (0, 0)
            };

            let mut st = state.borrow_mut();
            st.core = Some(AnalysisCore {
                path: path.clone(),
                info,
                structure: lime_analyze::Structure {
                    speech: vec![],
                    gaps: vec![],
                    duration_ms: info.duration_ms,
                    has_digital_silence: false,
                },
                chimes: vec![],
                chapters: chapters.clone(),
            });
            st.sentences = sents;

            if saved_ord > 0 && (saved_ord as usize) <= chapters.len() {
                ui.set_current((saved_ord as i32) - 1);
            }
            if saved_pos > 0 {
                ui.set_position(saved_pos as f32);
            }
        }
    }

    // ② 若无 .limed 文件，再检查本地 SQLite 数据库缓存
    if !cache_hit {
        if let Some(mid) = media_id {
            let st_ref = store.borrow();
            if let Ok(chapters) = st_ref.load_chapters(mid) {
                if !chapters.is_empty() {
                    cache_hit = true;
                    let sents = st_ref.load_sentences(mid).unwrap_or_default();
                    let rows = chapter_rows_from_list(&chapters);
                    ui.set_chapters(ModelRc::new(VecModel::from(rows)));
                    ui.set_has_chapters(!chapters.is_empty());
                    ui.set_duration(info.duration_ms as f32);

                    let n_mat = chapters
                        .iter()
                        .filter(|c| c.level == ChapterLevel::Material)
                        .count();
                    ui.set_chime_info(
                        format!("已加载本地分析 · 材料 {} / 题 {}", n_mat, chapters.len() - n_mat).into(),
                    );

                    if !sents.is_empty() {
                        ui.set_has_subtitles(true);
                        ui.set_status(
                            format!(
                                "⚡ 秒开就绪：{} 章节 · {} 句字幕",
                                chapters.len(),
                                sents.len()
                            )
                            .into(),
                        );
                    } else {
                        ui.set_status(format!("⚡ 秒开就绪：{} 章节（待转写字幕）", chapters.len()).into());
                    }

                    let (saved_pos, saved_ord) =
                        st_ref.get_progress(mid).unwrap_or(None).unwrap_or((0, 0));

                    let mut st = state.borrow_mut();
                    st.core = Some(AnalysisCore {
                        path: path.clone(),
                        info,
                        structure: lime_analyze::Structure {
                            speech: vec![],
                            gaps: vec![],
                            duration_ms: info.duration_ms,
                            has_digital_silence: false,
                        },
                        chimes: vec![],
                        chapters: chapters.clone(),
                    });
                    st.sentences = sents;

                    if saved_ord > 0 && (saved_ord as usize) <= chapters.len() {
                        ui.set_current((saved_ord as i32) - 1);
                    }
                    if saved_pos > 0 {
                        ui.set_position(saved_pos as f32);
                    }
                }
            }
        }
    }

    refresh_media_list(ui, &store.borrow(), &mut state.borrow_mut());

    if !cache_hit {
        ui.set_status(format!("已选择 {} · 正在切分…", filename).into());
        spawn_analyze(path, ui.as_weak(), tx.clone(), state.clone(), store.clone());
    }
}

fn refresh_media_list(ui: &MainWindow, store: &Store, state: &mut GuiState) {
    if let Ok(items) = store.list_media() {
        let rows: Vec<MediaRow> = items
            .iter()
            .map(|item| {
                let ratio = if item.duration_ms > 0 {
                    (item.pos_ms as f32 / item.duration_ms as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                MediaRow {
                    id: item.id as i32,
                    title: item.filename.clone().into(),
                    duration_str: crate::cli::fmt_ms(item.duration_ms).into(),
                    progress_ratio: ratio,
                    is_analyzed: item.is_analyzed,
                }
            })
            .collect();
        ui.set_media_list(ModelRc::new(VecModel::from(rows)));
        state.media_list = items;
    }
}

fn spawn_analyze(
    path: PathBuf,
    ui_weak: slint::Weak<MainWindow>,
    tx: Sender<Msg>,
    state: Rc<RefCell<GuiState>>,
    _store: Rc<RefCell<Store>>,
) {
    let busy = state.borrow().busy.clone();
    if busy.swap(true, Ordering::SeqCst) {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status("已有任务在跑…".into());
        }
        return;
    }
    if let Some(ui) = ui_weak.upgrade() {
        ui.set_status("① 切分中：解码 → 静音结构 → 叮咚…".into());
    }
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let r = analyze(&path, |m| {
            let _ = tx2.send(Msg::Status(m.to_string()));
        });
        match r {
            Ok(core) => match analyze_mono(&core.path) {
                Ok((mono, rate)) => {
                    let _ = tx.send(Msg::AnalyzeDone(Box::new(core), Box::new(mono), rate));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("解码失败：{e}")));
                }
            },
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("切分失败：{e}")));
            }
        }
        busy.store(false, Ordering::SeqCst);
    });
}

fn chapter_rows(core: &AnalysisCore) -> Vec<ChapterRow> {
    chapter_rows_from_list(&core.chapters)
}

fn chapter_rows_from_list(chapters: &[Chapter]) -> Vec<ChapterRow> {
    chapters
        .iter()
        .map(|c| ChapterRow {
            title: c.title.clone().into(),
            time_range: format!(
                "{} – {}",
                crate::cli::fmt_ms(c.start_ms),
                crate::cli::fmt_ms(c.end_ms)
            )
            .into(),
            level: if c.level == ChapterLevel::Material { 0 } else { 1 },
            confidence: c.confidence,
            low_confidence: c.confidence < 0.8,
            locked: c.locked,
        })
        .collect()
}

/// 把一句词折成多行（Slint 1.17 没有自动换行布局，按估算宽度折行）。
fn build_lines(words: &[Word]) -> ModelRc<ModelRc<WordCell>> {
    const MAX_PX: f32 = 600.0;
    fn width_of(s: &str) -> f32 {
        s.chars().count() as f32 * 12.5 + 24.0
    }
    let mut lines: Vec<Vec<WordCell>> = Vec::new();
    let mut cur: Vec<WordCell> = Vec::new();
    let mut cur_px = 0.0f32;
    for (i, w) in words.iter().enumerate() {
        let t = w.text.trim();
        if t.is_empty() {
            continue;
        }
        let px = width_of(t);
        if !cur.is_empty() && cur_px + px > MAX_PX {
            lines.push(std::mem::take(&mut cur));
            cur_px = 0.0;
        }
        cur.push(WordCell { text: t.into(), start_ms: w.start_ms as i32, idx: i as i32 });
        cur_px += px;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    let models: Vec<ModelRc<WordCell>> = lines
        .into_iter()
        .map(|l| ModelRc::new(VecModel::from(l)) as ModelRc<WordCell>)
        .collect();
    ModelRc::new(VecModel::from(models))
}

/// 更新逐词字幕（只有换句才重建模型，避免 20 fps 抖动）
fn update_subtitle(ui: &MainWindow, st: &mut GuiState, pos_ms: u64, last: &Cell<usize>) {
    if st.sentences.is_empty() {
        return;
    }
    let Some(i) = st
        .sentences
        .iter()
        .position(|s| pos_ms >= s.start_ms && pos_ms < s.end_ms + 400)
    else {
        return;
    };
    let s = &st.sentences[i];
    if last.get() != i {
        last.set(i);
        ui.set_lines(build_lines(&s.words));
        st.current_words = s.words.clone();
        let next = st.sentences.get(i + 1).map(|x| x.text.clone()).unwrap_or_default();
        ui.set_next_line(next.into());
    }
    let active = s
        .words
        .iter()
        .position(|w| pos_ms >= w.start_ms && pos_ms < w.end_ms + 120)
        .map(|x| x as i32)
        .unwrap_or(-1);
    ui.set_active_word(active);
}

pub fn file_label(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn fmt_srt_time(ms: u64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s / 60) % 60;
    let s = total_s % 60;
    let rem = ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{rem:03}")
}

fn fmt_lrc_time(ms: u64) -> String {
    let total_s = ms / 1000;
    let m = total_s / 60;
    let s = total_s % 60;
    let cs = (ms % 1000) / 10;
    format!("[{m:02}:{s:02}.{cs:02}]")
}

/// 诊断：打印工具/模型定位结果
pub fn tool_info() -> String {
    match (paths::find_whisper_exe(), paths::default_model()) {
        (Some(e), Some(m)) => format!("whisper: {}\nmodel:   {}", e.display(), m.display()),
        (None, _) => "whisper-cli.exe 未找到（应放在 tools/whisper/<cuda|blas>/Release/）".into(),
        (_, None) => "ggml 模型未找到（应放在 models/）".into(),
    }
}

