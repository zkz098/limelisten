//! 命令行模式：切分、断言、转写、引擎自测。GUI 与 CLI 共用同一套分析函数。

use crate::paths;
use anyhow::{anyhow, Result};
use lime_analyze::{
    build_chapters, detect_chimes, detect_structure, energy_envelope, snap::snap_words_to_speech,
    AnalyzeParams, Chime, GapKind, Structure,
};
use lime_asr::{group_sentences, WhisperCli};
use lime_audio::{render::write_wav, resample::LinearResampler, AudioInfo};
use lime_core::{Chapter, ChapterLevel, Sentence, Word};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 一次完整分析的结果（切分部分，不含 ASR）
pub struct AnalysisCore {
    pub path: PathBuf,
    pub info: AudioInfo,
    pub structure: Structure,
    pub chimes: Vec<Chime>,
    pub chapters: Vec<Chapter>,
}

/// 解码整文件为单声道 → 静音结构 → 叮咚 → 两级章节。
pub fn analyze(path: &Path, mut progress: impl FnMut(&str)) -> Result<AnalysisCore> {
    progress("解码音频…");
    let info = lime_audio::probe(path)?;
    let params = AnalyzeParams::default();
    let mut dec = lime_audio::decode::Decoder::open(path)?;
    let channels = dec.channels;
    let mut mono: Vec<f32> = Vec::with_capacity(info.duration_ms as usize * 8);
    while let Some(buf) = dec.next_packet()? {
        if channels <= 1 {
            mono.extend_from_slice(buf);
        } else {
            for frame in buf.chunks(channels) {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }
    }

    progress("检测静音结构…");
    let env = energy_envelope(&mono, info.sample_rate, &params);
    let mut structure = detect_structure(&env, info.sample_rate, &params);
    structure.has_digital_silence =
        !lime_analyze::digital_silence_spans(&mono, info.sample_rate, 500).is_empty();

    progress("检测叮咚标记…");
    let chimes = detect_chimes(&mono, info.sample_rate, &params.chime);

    progress("构建章节…");
    let chapters = build_chapters(&structure, &[], &chimes, &params);

    Ok(AnalysisCore {
        path: path.to_path_buf(),
        info,
        structure,
        chimes,
        chapters,
    })
}

/// 把已解码的单声道样本写成 16 kHz wav（Whisper 要求）。
pub fn dump_16k_wav(mono: &[f32], src_rate: u32, out: &Path) -> Result<()> {
    let target = 16_000u32;
    let samples: Vec<f32> = if src_rate == target {
        mono.to_vec()
    } else {
        let mut r = LinearResampler::new(src_rate, target, 1);
        let out_frames = (mono.len() as f64 * target as f64 / src_rate as f64) as usize;
        let mut y = vec![0.0f32; out_frames];
        let mut consumed = 0usize;
        let mut done = 0usize;
        while done < out_frames {
            let want = (out_frames - done).min(16_384);
            let need = r.needed_input_frames(want).min(mono.len() - consumed);
            if need < 2 {
                break;
            }
            let c = r.process(&mono[consumed..consumed + need], &mut y[done..done + want]);
            consumed += c;
            done += want;
        }
        y.truncate(done);
        y
    };
    write_wav(out, &samples, 1, target)?;
    Ok(())
}

/// 完整转写：16k wav → whisper 逐词 → 时间戳吸附 → 句子重组 → 归属章节
pub struct Transcript {
    /// 吸附前的原始词（诊断/对比用）
    pub raw_words: Vec<Word>,
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
    /// 用于吸附的地标对数（0 = 退化为仿射）
    pub landmarks: Vec<(u64, u64)>,
}

pub fn transcribe(
    core: &AnalysisCore,
    mono: &[f32],
    mut progress: impl FnMut(&str, f32),
) -> Result<Transcript> {
    let exe = paths::find_whisper_exe()
        .ok_or_else(|| anyhow!("找不到 whisper-cli.exe（请放到 tools/whisper/<cuda|blas>/Release/）"))?;
    let model = paths::default_model()
        .ok_or_else(|| anyhow!("找不到 ggml 模型（请放到 models/ 目录）"))?;

    let key = paths::cache_key(&core.path);
    let wav = paths::cache_dir().join(format!("{key}.16k.wav"));
    if !wav.is_file() {
        progress("导出 16 kHz 音频…", 0.02);
        dump_16k_wav(mono, core.info.sample_rate, &wav)?;
    }

    progress("Whisper 转写中…", 0.05);
    let cli = WhisperCli::new(exe, model);
    let out_base = paths::cache_dir().join(format!("{key}.asr"));
    let cancel = Arc::new(AtomicBool::new(false));
    let raw_words = cli.transcribe_words(&wav, &out_base, cancel, |p| {
        progress("Whisper 转写中…", 0.05 + p * 0.9);
    })?;

    progress("吸附时间戳…", 0.96);
    // 优先地标对齐（念白 "Text N" ↔ 叮咚 成对拟合），无足够地标时自动退化为仿射
    let landmarks = lime_analyze::snap::landmark_pairs(&raw_words, &core.structure, &core.chimes);
    let words = lime_analyze::snap::snap_with_landmarks(&raw_words, &core.structure, &core.chimes);

    progress("重组句子…", 0.98);
    let groups = group_sentences(&words, 0.4, 12, 8_000);
    let sentences: Vec<Sentence> = groups
        .iter()
        .filter_map(|idx| {
            let first = words.get(*idx.first()?)?;
            let last = words.get(*idx.last()?)?;
            let text = idx
                .iter()
                .filter_map(|i| words.get(*i))
                .map(|w| w.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return None;
            }
            Some(Sentence {
                start_ms: first.start_ms,
                end_ms: last.end_ms,
                text,
                words: idx.iter().filter_map(|i| words.get(*i).cloned()).collect(),
                chapter: None,
            })
        })
        .collect();

    Ok(Transcript { raw_words, words, sentences, landmarks })
}

/// 把句子归属到“题”级章节（没有题级则归属材料级）
pub fn assign_sentences_to_chapters(sentences: &mut [Sentence], chapters: &[Chapter]) {
    let q: Vec<(usize, u64, u64)> = chapters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.level == ChapterLevel::Question)
        .map(|(i, c)| (i, c.start_ms, c.end_ms))
        .collect();
    let m: Vec<(usize, u64, u64)> = chapters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.level == ChapterLevel::Material)
        .map(|(i, c)| (i, c.start_ms, c.end_ms))
        .collect();
    for s in sentences.iter_mut() {
        let mid = s.start_ms + (s.end_ms - s.start_ms) / 2;
        s.chapter = q
            .iter()
            .find(|(_, a, b)| mid >= *a && mid < *b)
            .or_else(|| m.iter().find(|(_, a, b)| mid >= *a && mid < *b))
            .map(|(i, _, _)| *i);
    }
}

// ============================ CLI 子命令 ============================

pub fn cmd_analyze(file: &str) -> Result<()> {
    let core = analyze(Path::new(file), |_m| {})?;
    println!("file: {file}");
    println!(
        "duration: {} ({:.1}s)  sr={} ch={}  digital_silence={}",
        fmt_ms(core.info.duration_ms),
        core.info.duration_ms as f64 / 1000.0,
        core.info.sample_rate,
        core.info.channels,
        core.structure.has_digital_silence
    );
    let n = |k: GapKind| core.structure.gaps.iter().filter(|g| g.kind == k).count();
    println!(
        "speech_islands: {}  gaps: {}  (material {} / question {} / sentence {})",
        core.structure.speech.len(),
        core.structure.gaps.len(),
        n(GapKind::Material),
        n(GapKind::Question),
        n(GapKind::Sentence)
    );
    println!("chimes: {}（叮 2578Hz → 咚 2039Hz，Δ≈0.59s）", core.chimes.len());
    for c in core.chimes.iter().take(40) {
        println!(
            "    叮咚 叮 {:>9} ({:6.1}dB) -> 咚 {:>9} ({:6.1}dB) conf={:.2}",
            fmt_ms(c.ding_ms),
            c.ding_db,
            fmt_ms(c.dong_ms),
            c.dong_db,
            c.confidence
        );
    }
    println!("章节:");
    for c in &core.chapters {
        let indent = if c.level == ChapterLevel::Material { "" } else { "    " };
        println!(
            "{indent}{:<12} {:>9} – {:>9}  conf={:.2}",
            c.title,
            fmt_ms(c.start_ms),
            fmt_ms(c.end_ms),
            c.confidence
        );
    }
    Ok(())
}

/// 自动断言（P0 验收口径，见 docs/PLAN.md §8）
pub fn cmd_assert(files: &[String]) -> Result<()> {
    let params = AnalyzeParams::default();
    let mut all = true;
    for f in files {
        let core = analyze(Path::new(f), |_| {})?;
        let rep = assertions(&core, &params);
        println!("==== {} ====", Path::new(f).file_name().unwrap_or_default().to_string_lossy());
        for l in &rep.lines {
            println!("  {l}");
        }
        println!("  => {}", if rep.pass { "PASS" } else { "FAIL" });
        all &= rep.pass;
    }
    if !all {
        std::process::exit(1);
    }
    Ok(())
}

pub struct AssertReport {
    pub pass: bool,
    pub lines: Vec<String>,
}

pub fn assertions(core: &AnalysisCore, params: &AnalyzeParams) -> AssertReport {
    use lime_analyze::collect_boundaries;
    let bounds = collect_boundaries(&core.structure, &core.chimes, params);
    let mut lines = Vec::new();
    let mut pass = true;

    // A. 每个材料边界都落在静音间隙内 或 与强叮咚对齐
    let mut a_ok = 0usize;
    for b in &bounds {
        let in_gap = core.structure.gaps.iter().any(|g| {
            g.kind != GapKind::Sentence && g.start_ms <= b.next_start_ms && g.end_ms >= b.next_start_ms
        });
        let near_chime = core
            .chimes
            .iter()
            .any(|c| (c.ding_ms as i64 - b.next_start_ms as i64).abs() <= 1500);
        if in_gap || near_chime {
            a_ok += 1;
        } else {
            lines.push(format!(
                "A FAIL: 边界 {} 既不在静音内也不靠叮咚",
                fmt_ms(b.next_start_ms)
            ));
        }
    }
    lines.push(format!("A: 边界定位合理 {a_ok}/{}（要求 100%）", bounds.len()));
    if a_ok != bounds.len() {
        pass = false;
    }

    // B. 章节不重叠、非空、覆盖首尾语音；材料间空隙必须属于答题间隔/叮咚
    let mats: Vec<&Chapter> = core
        .chapters
        .iter()
        .filter(|c| c.level == ChapterLevel::Material)
        .collect();
    let mut b_ok = true;
    for w in mats.windows(2) {
        if w[1].start_ms < w[0].end_ms {
            lines.push(format!("B FAIL: 材料重叠 {} -> {}", fmt_ms(w[0].end_ms), fmt_ms(w[1].start_ms)));
            b_ok = false;
            continue;
        }
        if w[1].start_ms > w[0].end_ms {
            let (a, b) = (w[0].end_ms, w[1].start_ms);
            let in_gap = core
                .structure
                .gaps
                .iter()
                .any(|g| g.kind != GapKind::Sentence && g.start_ms <= a && g.end_ms >= b);
            let has_chime = core
                .chimes
                .iter()
                .any(|c| c.ding_ms >= a && c.dong_ms <= b + 500);
            if !(in_gap || has_chime) {
                lines.push(format!("B FAIL: 材料间空隙不属于答题间隔/叮咚 {} – {}", fmt_ms(a), fmt_ms(b)));
                b_ok = false;
            }
        }
    }
    if core.chapters.iter().any(|c| c.end_ms <= c.start_ms) {
        lines.push("B FAIL: 存在空章节".into());
        b_ok = false;
    }
    let covered = mats.first().map(|m| m.start_ms).unwrap_or(0)
        == core.structure.speech.first().map(|s| s.0).unwrap_or(0)
        && mats.last().map(|m| m.end_ms).unwrap_or(0)
            == core.structure.speech.last().map(|s| s.1).unwrap_or(core.info.duration_ms);
    lines.push(format!("B: 不重叠+非空+覆盖语音首尾 {}", if covered && b_ok { "是" } else { "否" }));
    pass &= b_ok && covered;

    // C. 强叮咚与边界重合率
    let strong: Vec<&Chime> = core
        .chimes
        .iter()
        .filter(|c| c.confidence >= params.chime.min_confidence)
        .collect();
    let mut c_ok = 0usize;
    for ch in &strong {
        if bounds.iter().any(|b| (b.next_start_ms as i64 - ch.ding_ms as i64).abs() <= 1500) {
            c_ok += 1;
        }
    }
    let rate = if strong.is_empty() { 1.0 } else { c_ok as f32 / strong.len() as f32 };
    lines.push(format!(
        "C: 叮咚与边界重合 {c_ok}/{} = {:.0}%（要求 ≥80%）",
        strong.len(),
        rate * 100.0
    ));
    if rate < 0.8 {
        pass = false;
    }

    lines.push(format!(
        "汇总: 时长 {} / 材料 {} / 题 {} / 叮咚 {}（强 {}）",
        fmt_ms(core.info.duration_ms),
        mats.len(),
        core.chapters.len() - mats.len(),
        core.chimes.len(),
        strong.len()
    ));
    AssertReport { pass, lines }
}

/// 只跑 ASR 链路（不启 GUI）：打印前 N 句 + 吸附前后对比
pub fn cmd_transcribe(file: &str, show: usize) -> Result<()> {
    let core = analyze(Path::new(file), |m| eprintln!("[analysis] {m}"))?;
    // 重新解码取单声道样本（避免 analyze 返回值过大）
    let info = crate::cli::analyze_mono(Path::new(file))?;
    let t0 = std::time::Instant::now();
    let tr = transcribe(&core, &info.0, |m, p| eprintln!("[asr {:.0}%] {m}", p * 100.0))?;
    println!(
        "转写完成：{} 原始词 → {} 吸附词 → {} 句，地标对数 {}，用时 {:?}",
        tr.raw_words.len(),
        tr.words.len(),
        tr.sentences.len(),
        tr.landmarks.len(),
        t0.elapsed()
    );
    println!("\n=== 地标诊断 ===");
    println!("{}", lime_analyze::snap::landmark_diagnosis(&tr.raw_words, &core.chimes));
    if !tr.landmarks.is_empty() {
        println!("\n=== 地标配对（念白 Text-N ↔ 叮咚）===");
        for (w, r) in tr.landmarks.iter().take(12) {
            println!("   whisper {:>9} ms  ↔  真实 {:>9} ms", w, r);
        }
    }
    println!("\n=== 吸附前 vs 吸附后（前 12 个词）===");
    for (raw, sn) in tr.raw_words.iter().zip(tr.words.iter()).take(12) {
        println!(
            "  {:>9} → {:>9} ms  {:.1}s→{:.1}s  {:?}",
            raw.start_ms,
            sn.start_ms,
            raw.start_ms as f64 / 1000.0,
            sn.start_ms as f64 / 1000.0,
            raw.text.trim()
        );
    }

    // 三种吸附策略对比（用“念白标记”+ 叮咚当锚点判定谁更准）
    let mk = |m| lime_analyze::snap::snap_by_mode(&tr.raw_words, &core.structure, m);
    let by_index = mk(lime_analyze::snap::SnapMode::Index);
    let by_ratio = mk(lime_analyze::snap::SnapMode::Ratio);
    let by_affine = mk(lime_analyze::snap::SnapMode::Affine);
    let by_landmark = &tr.words;
    if let Some(f) = lime_analyze::snap::fit_affine(&tr.raw_words, &core.structure) {
        println!(
            "\n仿射拟合: t_real = {:.0} ms + {:.4} × t_whisper  （k→1 表示 Whisper 时间轴几乎无需拉伸）",
            f.a_ms, f.k
        );
    }
    println!("\n=== 吸附策略对比（只列带 text/one/two/three 的词）===");
    println!(
        "   {:>12} {:>8} {:>9} {:>9} {:>9} {:>9}   {}",
        "raw(ms)", "(s)", "index", "ratio", "affine", "地标", "text"
    );
    let mut shown = 0;
    for i in 0..tr.raw_words.len() {
        let raw = &tr.raw_words[i];
        let l = raw.text.to_ascii_lowercase();
        if !(l.contains("text") || l.contains("one") || l.contains("two") || l.contains("three")) {
            continue;
        }
        println!(
            "   {:>12} {:>8.1} {:>9} {:>9} {:>9} {:>9}   {}",
            raw.start_ms,
            raw.start_ms as f64 / 1000.0,
            by_index[i].start_ms,
            by_ratio[i].start_ms,
            by_affine[i].start_ms,
            by_landmark[i].start_ms,
            raw.text.trim()
        );
        shown += 1;
        if shown >= 14 {
            break;
        }
    }
    println!(
        "\n（本文件强叮咚时刻(ms)：{:?}）",
        core.chimes
            .iter()
            .filter(|c| c.confidence >= 0.35)
            .map(|c| c.ding_ms)
            .take(8)
            .collect::<Vec<_>>()
    );
    println!("\n=== 句子（前 {show} 句）===");
    let mut sentences = tr.sentences.clone();
    assign_sentences_to_chapters(&mut sentences, &core.chapters);
    for s in sentences.iter().take(show) {
        println!(
            "  [{:>9} – {:>9}]  chapter={:?}  {}",
            fmt_ms(s.start_ms),
            fmt_ms(s.end_ms),
            s.chapter,
            s.text
        );
    }
    Ok(())
}

/// 解码为单声道样本（分析/转写共用）
pub fn analyze_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let info = lime_audio::probe(path)?;
    let mut dec = lime_audio::decode::Decoder::open(path)?;
    let channels = dec.channels;
    let mut mono: Vec<f32> = Vec::with_capacity(info.duration_ms as usize * 8);
    while let Some(buf) = dec.next_packet()? {
        if channels <= 1 {
            mono.extend_from_slice(buf);
        } else {
            for frame in buf.chunks(channels) {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }
    }
    Ok((mono, info.sample_rate))
}

pub fn fmt_ms(ms: u64) -> String {
    let total = ms / 1000;
    let (m, s) = (total / 60, total % 60);
    format!("{m:02}:{s:02}.{:01}", (ms % 1000) / 100)
}
