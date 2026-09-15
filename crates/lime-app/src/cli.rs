//! 命令行模式：切分、断言、转写、引擎自测。GUI 与 CLI 共用同一套分析函数。

#[cfg(feature = "whisper")]
use crate::paths;
use anyhow::{anyhow, Result};
use lime_analyze::{
    build_chapters, detect_chimes, detect_structure, energy_envelope,
    AnalyzeParams, Chime, GapKind, Structure,
};
#[cfg(feature = "whisper")]
use lime_asr::{group_sentences, WhisperCli};
#[cfg(feature = "whisper")]
use lime_audio::{render::write_wav, resample::LinearResampler};
use lime_audio::AudioInfo;
use lime_core::{Chapter, ChapterLevel, LimedFile, LimedMeta, Sentence, Word};
use lime_store::Store;
use std::path::{Path, PathBuf};
#[cfg(feature = "whisper")]
use std::sync::atomic::AtomicBool;
#[cfg(feature = "whisper")]
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

    progress("分析音频分段…");
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
#[cfg(feature = "whisper")]
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
#[allow(dead_code)]
pub struct Transcript {
    /// 吸附前的原始词（诊断/对比用）
    pub raw_words: Vec<Word>,
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
    /// 用于吸附的地标对数（0 = 退化为仿射）
    pub landmarks: Vec<(u64, u64)>,
}

#[cfg(feature = "whisper")]
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

#[cfg(not(feature = "whisper"))]
pub fn transcribe(
    _core: &AnalysisCore,
    _mono: &[f32],
    _progress: impl FnMut(&str, f32),
) -> Result<Transcript> {
    Err(anyhow!("当前为 Slim 纯听版本（未包含 Whisper 引擎）。请使用完整版预生成 .limed 缓存文件。"))
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
    print_chapter_tree(&core.chapters, usize::MAX);
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

#[cfg(not(feature = "whisper"))]
pub fn cmd_transcribe(_file: &str, _show: usize) -> Result<()> {
    Err(anyhow!("当前为 Slim 纯听版（未编译 Whisper 引擎）。请使用完整版进行转写，或使用 --show-limed 查看 .limed 文件。"))
}

/// 只跑 ASR 链路（不启 GUI）：打印前 N 句 + 吸附前后对比
#[cfg(feature = "whisper")]
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
    println!("{}", lime_analyze::snap::landmark_diagnosis(&tr.raw_words, &core.structure, &core.chimes));
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

/// 打印两级章节树（与左侧导航同构，用于核对材料/题是否归位）。
pub fn print_chapter_tree(chapters: &[Chapter], limit: usize) {
    let n_mat = chapters.iter().filter(|c| c.is_material()).count();
    println!(
        "  共 {} 节：{} 材料 / {} 题 · 规范顺序 {}",
        chapters.len(),
        n_mat,
        chapters.len() - n_mat,
        if lime_core::chapters_are_canonical(chapters) { "✓" } else { "✗ (需要 normalize)" }
    );
    for c in chapters.iter().take(limit) {
        let indent = if c.is_material() { "" } else { "    " };
        println!(
            "  {indent}{:?} {:<22} {:>9} – {:>9}  conf={:.2}{}",
            c.seq,
            c.title,
            fmt_ms(c.start_ms),
            fmt_ms(c.end_ms),
            c.confidence,
            if c.locked { " [人工锁定]" } else { "" }
        );
    }
    if chapters.len() > limit {
        println!("  ... 以及其他 {} 个章节", chapters.len() - limit);
    }
}

/// `--chapters <音频>`：按左侧导航的读法（本地库 chapter 表）打印两级章节树。
///
/// 用途：不启动 GUI 也能核对「材料 → 其题」是否归位（旧版这里会把所有材料挤到最前、
/// 把所有材料的"第 1 题"排在一起）。首次运行会顺便把老库迁移到 V3（补 `chapter.seq`）。
pub fn cmd_chapters(file: &str) -> Result<()> {
    let path = std::fs::canonicalize(file).map_err(|e| anyhow!("找不到文件 {file}: {e}"))?;
    let full = path.to_string_lossy().to_string();
    // Windows canonicalize 会带上 `\\?\` 扩展前缀，而入库时存的是普通路径
    let key = full.strip_prefix(r"\\?\").unwrap_or(&full).to_string();
    let db_path = crate::paths::data_dir().join("library.db");
    if !db_path.is_file() {
        return Err(anyhow!(
            "本地库还不存在（{}）；先在 GUI 里打开一次该音频，或跑 --limed 生成缓存",
            db_path.display()
        ));
    }
    let store = Store::open(&db_path).map_err(|e| anyhow!("{e}"))?;
    let media_id = match store.get_media_by_path(&key).map_err(|e| anyhow!("{e}"))? {
        Some(m) => Some(m.id),
        None => {
            // 文件被挪过位置时，退而求其次按文件名匹配
            let name = path.file_name().map(|n| n.to_string_lossy().to_string());
            let all = store.list_media().map_err(|e| anyhow!("{e}"))?;
            all.into_iter()
                .filter(|m| name.as_deref().is_some_and(|n| m.filename == n))
                .max_by_key(|m| m.id)
                .map(|m| m.id)
        }
    };
    let media_id = media_id.ok_or_else(|| {
        anyhow!(
            "本地库还没有 {}：先在 GUI 里打开一次（会自动入库）",
            path.display()
        )
    })?;
    let chapters = store.load_chapters(media_id).map_err(|e| anyhow!("{e}"))?;

    println!("==== 本地库章节树: {} ====", path.display());
    println!("  库文件: {}", db_path.display());
    if chapters.is_empty() {
        println!("  尚无章节（需要先跑 ① 切分，或导入同名 .limed）");
        return Ok(());
    }
    print_chapter_tree(&chapters, usize::MAX);
    Ok(())
}

/// 预先完成切分和转译，并压缩保存为 .limed 缓存文件。
pub fn cmd_export_limed(file: &str) -> Result<PathBuf> {
    let path = Path::new(file);
    if !path.is_file() {
        return Err(anyhow!("音频文件不存在: {file}"));
    }
    println!("==== 开始预处理并打包 .limed 缓存: {} ====", path.display());
    let core = analyze(path, |m| println!("  [切分] {m}"))?;
    let (mono, _) = analyze_mono(path)?;

    #[cfg(feature = "whisper")]
    let sentences = {
        println!("  正在执行 Whisper 逐词转写与时间戳吸附...");
        let tr = transcribe(&core, &mono, |m, p| {
            println!("    [转写进度 {:.0}%] {m}", p * 100.0);
        })?;
        let mut sents = tr.sentences;
        assign_sentences_to_chapters(&mut sents, &core.chapters);
        sents
    };
    #[cfg(not(feature = "whisper"))]
    let sentences = {
        let _ = mono;
        println!("  [提示] 当前为 Slim 版本，跳过 Whisper 转译，仅打包切分章节信息。");
        Vec::new()
    };

    let meta = LimedMeta {
        audio_filename: path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        duration_ms: core.info.duration_ms,
        sample_rate: core.info.sample_rate,
        channels: core.info.channels,
        file_size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        version: 1,
        generator: format!("limelisten {}", env!("CARGO_PKG_VERSION")),
    };

    let limed = LimedFile::new(meta, lime_core::normalize_chapters(&core.chapters), sentences);
    let out_path = path.with_extension("limed");
    limed.save(&out_path)?;

    let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    println!("\n✅ 成功生成 .limed 缓存文件: {}", out_path.display());
    println!("  章节总数: {}", limed.chapters.len());
    println!("  字幕句数: {}", limed.sentences.len());
    println!(
        "  文件大小: {:.2} KB (zstd 压缩)",
        size_bytes as f64 / 1024.0
    );
    Ok(out_path)
}

/// 查看 .limed 缓存文件内容。
pub fn cmd_show_limed(file: &str) -> Result<()> {
    let path = Path::new(file);
    if !path.is_file() {
        return Err(anyhow!("文件不存在: {file}"));
    }
    let limed = LimedFile::load(path)?;
    // 外部工具生成的缓存也可能顺序错乱：打印前先规范化，保证两级层次正确
    let chapters = lime_core::normalize_chapters(&limed.chapters);
    println!("==== LIMED 缓存信息: {} ====", path.display());
    println!("  关联音频: {}", limed.meta.audio_filename);
    println!(
        "  音频时长: {} ({:.1}s)",
        fmt_ms(limed.meta.duration_ms),
        limed.meta.duration_ms as f64 / 1000.0
    );
    println!(
        "  采样参数: {} Hz | {} 声道",
        limed.meta.sample_rate, limed.meta.channels
    );
    println!("  原文件大小: {} 字节", limed.meta.file_size);
    println!("  生成工具: {}", limed.meta.generator);
    println!("\n[章节结构] 共 {} 节:", chapters.len());
    print_chapter_tree(&chapters, 15);
    println!("\n[字幕预览] 共 {} 句:", limed.sentences.len());
    for s in limed.sentences.iter().take(8) {
        println!("  [{:>9} – {:>9}] {}", fmt_ms(s.start_ms), fmt_ms(s.end_ms), s.text);
    }
    if limed.sentences.len() > 8 {
        println!("  ... 以及其他 {} 句字幕", limed.sentences.len() - 8);
    }
    Ok(())
}

#[cfg(not(feature = "whisper"))]
pub fn cmd_benchmark(_sample_path: Option<&str>) -> Result<()> {
    println!("当前运行的是 Slim 纯听版（已裁剪 Whisper 引擎）。");
    println!("Slim 版无需在本地运行模型转写，可直接消费全功能版生成的 .limed 缓存。");
    println!("如需评测机器的 Whisper 转写性能，请使用全功能版运行 `limelisten --bench`。");
    Ok(())
}

#[cfg(feature = "whisper")]
#[derive(Debug)]
enum BenchResult {
    Success { elapsed: f64, speedup: f64 },
    Timeout,
    Missing,
    Error(String),
}

#[cfg(feature = "whisper")]
pub fn prepare_benchmark_wav(sample_path: Option<&str>) -> Result<(PathBuf, f64)> {
    let cache_dir = paths::data_dir().join("cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let bench_wav = cache_dir.join("benchmark_15s.16k.wav");

    let src = if let Some(p) = sample_path {
        PathBuf::from(p)
    } else {
        let candidates = [
            PathBuf::from("samples/训练2.mp3"),
            PathBuf::from("samples/训练1.mp3"),
            PathBuf::from("samples/听力音频1.mp3"),
            PathBuf::from("samples/9.2听力练习一.mp3"),
            PathBuf::from(".tools/clip2min.wav"),
        ];
        candidates
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| {
                anyhow!("未找到可用于测速的样本音频，请通过 `limelisten --bench <音频>` 指定音频文件")
            })?
    };

    let info = lime_audio::probe(&src)?;
    let mut dec = lime_audio::decode::Decoder::open(&src)?;
    let channels = dec.channels;
    let target_frames = (info.sample_rate as usize) * 15; // 15 秒
    let mut mono: Vec<f32> = Vec::with_capacity(target_frames);

    while mono.len() < target_frames {
        if let Some(buf) = dec.next_packet()? {
            if channels <= 1 {
                let take = (target_frames - mono.len()).min(buf.len());
                mono.extend_from_slice(&buf[..take]);
            } else {
                for frame in buf.chunks(channels) {
                    if mono.len() >= target_frames {
                        break;
                    }
                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                }
            }
        } else {
            break;
        }
    }

    let actual_dur_s = mono.len() as f64 / info.sample_rate as f64;
    if actual_dur_s < 3.0 {
        return Err(anyhow!("样本音频过短（需至少 3 秒）: {actual_dur_s:.1}s"));
    }

    dump_16k_wav(&mono, info.sample_rate, &bench_wav)?;
    Ok((bench_wav, actual_dur_s))
}

#[cfg(feature = "whisper")]
pub fn cmd_benchmark(sample_path: Option<&str>) -> Result<()> {
    println!("============================= Whisper 转写性能测速 (3:1 标准) =============================");
    println!("正在准备 15 秒基准测试音频...");
    let (test_wav, audio_dur_s) = prepare_benchmark_wav(sample_path)?;
    println!(
        "基准音频准备完毕: {} (时长: {:.1}s)",
        test_wav.display(),
        audio_dur_s
    );

    let exe = paths::find_whisper_exe().ok_or_else(|| {
        anyhow!("找不到 whisper-cli.exe（请放置在 tools/whisper/ 或 .tools/ 目录）")
    })?;
    println!("Whisper 引擎: {}", exe.display());
    println!("判定标准: 3:1 标准 (3 分钟音频需 1 分钟以内完成, 即倍速比 S >= 3.0x, 15s 耗时 <= 5.0s)");
    println!(
        "超时熔断: 若转写耗时 > {:.1}s (RTF > 1.0, 慢于原声播放), 将主动终止并跳至下一规格\n",
        audio_dur_s
    );

    let models = [
        ("turbo", "Turbo (large-v3)"),
        ("small", "Small (.en)"),
        ("base", "Base (.en)"),
        ("tiny", "Tiny (.en)"),
    ];

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut results = Vec::new();

    for (spec, label) in &models {
        println!("---------------------------------------------------------------------------------------");
        let model_path = match paths::find_model_by_spec(spec) {
            Some(p) => p,
            None => {
                println!(">> 规格 [{label}]: [未检测到模型文件，跳过]");
                results.push((*spec, *label, BenchResult::Missing, None));
                continue;
            }
        };

        let model_name = model_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(spec);
        println!(">> 正在测速 [{label}] (模型: {model_name})...");

        let out_base = paths::cache_dir().join(format!("bench_{spec}"));
        let start = std::time::Instant::now();

        let mut child = std::process::Command::new(&exe)
            .arg("-m")
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
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("spawn whisper failed: {e}"))?;

        let mut timed_out = false;
        let mut child_err = None;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        child_err = Some(format!("whisper exit code: {status}"));
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
            println!(
                "   ⚠️ 耗时超过音频时长 ({:.1}s)！RTF > 1.0 (慢于原声播放)，触发超时熔断并主动终止！",
                audio_dur_s
            );
            results.push((
                *spec,
                *label,
                BenchResult::Timeout,
                Some(model_name.to_string()),
            ));
        } else if let Some(e) = child_err {
            println!("   ❌ 运行失败: {e}");
            results.push((
                *spec,
                *label,
                BenchResult::Error(e),
                Some(model_name.to_string()),
            ));
        } else {
            let elapsed = start.elapsed().as_secs_f64();
            let speedup = audio_dur_s / elapsed;
            let verdict = if speedup >= 10.0 {
                "★★★★★ 极速推荐 (远超 3:1)"
            } else if speedup >= 3.0 {
                "★★★☆☆ 达标推荐 (符合 3:1)"
            } else {
                "★☆☆☆☆ 不建议 (低于 3:1 标准，转译偏慢)"
            };
            println!("   耗时: {elapsed:.2}s | 转写倍速: {speedup:.1}x | 评定: {verdict}");
            results.push((
                *spec,
                *label,
                BenchResult::Success { elapsed, speedup },
                Some(model_name.to_string()),
            ));
        }
    }

    println!("\n================================== 测速评估汇总与推荐 ==================================");
    println!(
        "{:<8} {:<18} {:<24} {:<10} {:<10} {:<16}",
        "规格", "型号名称", "本地文件", "实测耗时", "转写倍速", "3:1标准判定"
    );
    println!("----------------------------------------------------------------------------------------");

    let mut best_spec = None;
    let mut best_speedup = 0.0f64;

    for (spec, label, res, model_name) in &results {
        let fname = model_name.as_deref().unwrap_or("未找到");
        match res {
            BenchResult::Success { elapsed, speedup } => {
                let pass_str = if *speedup >= 3.0 {
                    "✅ 达标推荐"
                } else {
                    "⚠️ 不建议"
                };
                println!(
                    "{:<8} {:<18} {:<24} {:<9.2}s {:<9.1}x {:<16}",
                    spec, label, fname, elapsed, speedup, pass_str
                );
                if *speedup >= 3.0 && best_spec.is_none() {
                    best_spec = Some((*spec, *label, *speedup));
                }
                if *speedup > best_speedup {
                    best_speedup = *speedup;
                }
            }
            BenchResult::Timeout => {
                println!(
                    "{:<8} {:<18} {:<24} {:<10} {:<10} {:<16}",
                    spec, label, fname, "> 15.0s", "< 1.0x", "❌ 超时熔断"
                );
            }
            BenchResult::Missing => {
                println!(
                    "{:<8} {:<18} {:<24} {:<10} {:<10} {:<16}",
                    spec, label, fname, "--", "--", "[未下载]"
                );
            }
            BenchResult::Error(e) => {
                println!(
                    "{:<8} {:<18} {:<24} {:<10} {:<10} {:<16}",
                    spec, label, fname, "Error", "--", e
                );
            }
        }
    }

    println!("----------------------------------------------------------------------------------------");
    println!("【硬件适配最终建议】:");
    if let Some((spec, label, speedup)) = best_spec {
        println!("👉 推荐规格: [{label}] (代号: {spec})");
        println!(
            "   本机转写倍速可达 {speedup:.1}x（远快于 3:1 标准），处理 15 分钟听力仅需约 {:.1} 秒，体验极佳！",
            900.0 / speedup
        );
    } else if best_speedup > 0.0 {
        println!("👉 本机测得的最高倍速仅为 {best_speedup:.1}x（未达到 3:1 标准要求）。");
        println!("   建议：由于转写耗时较长，建议使用更轻量的 base/tiny 模型，或使用 Slim 纯听版配合预生成的 .limed 缓存文件。");
    } else {
        println!("👉 未检测到可正常运行的模型，或当前环境速度过慢已全部熔断。");
        println!("   建议使用 Slim 纯听版 (`limelisten-slim.exe`)，直接免转译加载 .limed 缓存。");
    }
    println!("========================================================================================\n");
    Ok(())
}

