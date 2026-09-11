//! GUI 装配：Slint 界面 ↔ 后台分析线程 ↔ 播放引擎。
//!
//! 线程模型（P0）：
//! - 后台线程只做 CPU 活（解码 / 静音结构 / 叮咚 / ASR），结果经 channel 回传；
//! - 界面状态与播放引擎都留在 UI 线程，定时器 50 ms 轮询（引擎进度是原子量，直接读）；
//! - 切分与转写互斥（busy 标志），避免两个 CPU 任务抢满机器。

use crate::cli::{analyze, analyze_mono, assign_sentences_to_chapters, transcribe, AnalysisCore};
use crate::paths;
use crate::{ChapterRow, MainWindow, WordCell};
use crossbeam_channel::{unbounded, Receiver, Sender};
use lime_audio::Engine;
use lime_core::{ChapterLevel, Sentence, Word};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
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
}

#[derive(Default)]
struct GuiState {
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

pub fn run() -> anyhow::Result<()> {
    let ui = MainWindow::new()?;
    let state = Rc::new(RefCell::new(GuiState {
        speed: 1.0,
        auto_pause: true,
        ..Default::default()
    }));
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = unbounded();
    let last_sentence = Rc::new(Cell::new(usize::MAX));

    // ---------------- 打开文件 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        let last_sentence = Rc::clone(&last_sentence);
        ui.on_open_file(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(path) = rfd::FileDialog::new()
                .add_filter("音频", &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus"])
                .pick_file()
            else {
                return;
            };
            {
                let mut st = state.borrow_mut();
                st.path = Some(path.clone());
                st.core = None;
                st.sentences.clear();
                st.current_words.clear();
                st.engine = None; // 旧引擎随 drop 关闭输出流
            }
            last_sentence.set(usize::MAX);
            ui.set_current(-1);
            ui.set_lines(ModelRc::new(VecModel::from(Vec::<ModelRc<WordCell>>::new())));
            ui.set_active_word(-1);
            ui.set_has_subtitles(false);
            ui.set_next_line("".into());
            ui.set_chime_info("".into());
            ui.set_playing(false);
            ui.set_position(0.0);
            ui.set_status(format!("已选择 {}", file_label(&path)).into());
            spawn_analyze(path, ui_weak.clone(), tx.clone(), state.clone());
        });
    }

    // ---------------- ① 切分 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        ui.on_analyze(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(path) = state.borrow().path.clone() else {
                ui.set_status("请先打开一个音频文件".into());
                return;
            };
            spawn_analyze(path, ui_weak.clone(), tx.clone(), state.clone());
        });
    }

    // ---------------- ② 转写 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let tx = tx.clone();
        ui.on_transcribe(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
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
                ui.set_status("请先点“① 切分”（转写需要语音岛来吸附时间戳）".into());
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
                        ui.set_status("播放引擎已建立".into());
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

    ui.on_export_srt(|| {}); // TODO(P2)

    // ---------------- 定时器 ----------------
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
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
                    Msg::TranscribeDone(sentences) => {
                        ui.set_transcribe_progress(-1.0);
                        ui.set_has_subtitles(true);
                        ui.set_status(format!("② 转写完成：{} 句字幕", sentences.len()).into());
                        last_sentence.set(usize::MAX);
                        state.borrow_mut().sentences = sentences;
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
                        let mut st = state.borrow_mut();
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
                            ui.set_info("已暂停到下一题".into());
                        }
                    }
                }
            }

            // 字幕
            if ui.get_has_subtitles() {
                let mut st = state.borrow_mut();
                update_subtitle(&ui, &mut st, pos, &last_sentence);
            }
        });
        std::mem::forget(timer); // 保持存活
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

fn spawn_analyze(
    path: PathBuf,
    ui_weak: slint::Weak<MainWindow>,
    tx: Sender<Msg>,
    state: Rc<RefCell<GuiState>>,
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
    core.chapters
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
        })
        .collect()
}

/// 把一句词折成多行（Slint 1.17 没有自动换行布局，这里按估算宽度手动折行）。
/// 22px 字号下英文字符平均宽约 11px；容器约等于窗口宽 - 左栅 - 内边距。
fn build_lines(words: &[Word]) -> ModelRc<ModelRc<WordCell>> {
    const MAX_PX: f32 = 820.0;
    fn width_of(s: &str) -> f32 {
        s.chars().count() as f32 * 11.0 + 6.0
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

/// 诊断：打印工具/模型定位结果
pub fn tool_info() -> String {
    match (paths::find_whisper_exe(), paths::default_model()) {
        (Some(e), Some(m)) => format!("whisper: {}\nmodel:   {}", e.display(), m.display()),
        (None, _) => "whisper-cli.exe 未找到（应放在 tools/whisper/<cuda|blas>/Release/）".into(),
        (_, None) => "ggml 模型未找到（应放在 models/）".into(),
    }
}
