//! Whisper 驱动：调用 whisper-cli.exe（子进程）→ 解析 JSON → 词级时间戳 → 句子重组。
//!
//! 关键实测结论（docs/GRILLING.md §2.5/§2.4 与 U1）：
//! - 必须用 `-ml 1 -sow` 逐词模式：普通模式时间戳只有 1 s 粒度，逐词模式 10 ms。
//! - 不要用 `-dtw`：会关闭 flash attention 并让时间戳退化成整秒。
//! - `(bell chimes)` 标签**不可靠**（同一段音频单独转写时不再出现，被并入整句），
//!   因此标记识别的主信号是"空白长词段 + 静音结构"，文本标签仅作可选提示。

use lime_analyze::{Anchor, AnchorKind};
use lime_core::{Error, Result, Word};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct WhisperCli {
    pub exe: PathBuf,
    pub model: PathBuf,
    pub threads: usize,
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub percent: f32,
}

impl WhisperCli {
    pub fn new(exe: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        Self { exe: exe.into(), model: model.into(), threads, language: "en".into() }
    }

    /// 逐词转写。`out_base` 为输出文件前缀（生成 `<base>.json`）。
    /// `cancel` 置位时杀死子进程并返回已取消错误。
    pub fn transcribe_words(
        &self,
        wav: &Path,
        out_base: &Path,
        cancel: Arc<AtomicBool>,
        mut on_progress: impl FnMut(f32),
    ) -> Result<Vec<Word>> {
        // 注意：whisper-cli 是**追加** .json（x.asr → x.asr.json），
        // 而 Path::with_extension 是**替换**扩展名（x.asr → x.json）—— 用错就会找不到文件。
        let json_path = |base: &Path| -> PathBuf {
            let mut s = base.as_os_str().to_os_string();
            s.push(".json");
            PathBuf::from(s)
        };
        let _ = std::fs::remove_file(json_path(out_base));
        let mut child = Command::new(&self.exe)
            .arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(wav)
            .arg("-l")
            .arg(&self.language)
            .arg("-t")
            .arg(self.threads.to_string())
            .arg("-oj")
            .arg("-ml")
            .arg("1")
            .arg("-sow")
            .arg("-pp")
            .arg("-of")
            .arg(out_base)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Asr(format!("spawn {}: {e}", self.exe.display())))?;

        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(std::result::Result::ok) {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    return Err(Error::Asr("cancelled".into()));
                }
                if let Some(p) = parse_progress(&line) {
                    on_progress(p);
                }
            }
        }
        let status = child.wait().map_err(|e| Error::Asr(e.to_string()))?;
        if !status.success() {
            return Err(Error::Asr(format!("whisper-cli exited with {status}")));
        }
        let json = json_path(out_base);
        let raw = std::fs::read_to_string(&json)
            .map_err(|e| Error::Asr(format!("read {}: {e}", json.display())))?;
        parse_words_json(&raw)
    }
}

/// 解析 `whisper_print_progress_callback: progress = 42%` 这类进度行。
fn parse_progress(line: &str) -> Option<f32> {
    let idx = line.find("progress")?;
    let rest = &line[idx..];
    let pct_idx = rest.find('=')?;
    let num: String = rest[pct_idx + 1..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse::<f32>().ok().map(|p| p / 100.0)
}

/// 解析 whisper.cpp `-oj` 输出（顶层 `transcription` 数组，`offsets` 为毫秒）。
pub fn parse_words_json(raw: &str) -> Result<Vec<Word>> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| Error::Asr(format!("json: {e}")))?;
    let arr = v
        .get("transcription")
        .and_then(|t| t.as_array())
        .ok_or_else(|| Error::Asr("missing `transcription`".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let from = item.pointer("/offsets/from").and_then(|x| x.as_u64()).unwrap_or(0);
        let to = item.pointer("/offsets/to").and_then(|x| x.as_u64()).unwrap_or(from);
        let text = item.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
        out.push(Word { start_ms: from, end_ms: to, text });
    }
    Ok(out)
}

/// 句子重组：标点 + 词间停顿（默认 0.4 s）切句，单句 ≤ max_words / ≤ max_ms。
/// 依据 docs/PLAN.md §4-⑧。
pub fn group_sentences(words: &[Word], gap_s: f32, max_words: usize, max_ms: u64) -> Vec<Vec<usize>> {
    let gap_ms = (gap_s * 1000.0) as u64;
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let text = w.text.trim();
        if text.is_empty() {
            continue;
        }
        let prev_end = cur.last().map(|&j| words[j].end_ms).unwrap_or(w.start_ms);
        let pause = w.start_ms.saturating_sub(prev_end);
        let long_pause = !cur.is_empty() && pause >= gap_ms;
        let too_long = cur.len() >= max_words
            || (!cur.is_empty() && w.end_ms - words[cur[0]].start_ms > max_ms);
        let ends_sentence =
            text.ends_with('.') || text.ends_with('?') || text.ends_with('!') || text.ends_with('。');
        if !cur.is_empty() && (long_pause || too_long) {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(i);
        if ends_sentence || cur.len() >= max_words {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 语义锚点（**仅作提示/命名**，不作为主依据 —— 主依据是静音结构 + 叮咚检测）。
/// 注：实测 `(bell chimes)` 是数字静音上的幻觉（GRILLING §2.2），故不在此处采用。
pub fn anchors_from_words(words: &[Word]) -> Vec<Anchor> {
    // 备注：念白可能把数字念出来（"Text one"）也可能是数字（"Text 1"）
    let re_material = regex::Regex::new(
        r"(?i)\b(text|unit|lesson|passage)\s*(?:number\s*)?(\d+|[a-d]|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)\b",
    )
    .ok();
    let re_question = regex::Regex::new(r"(?i)\bquestion\s*(?:number\s*)?(\d+)\b").ok();
    let re_cn_question = regex::Regex::new(r"第\s*(\d+)\s*题").ok();
    let mut out = Vec::new();
    // 合并连续词文本，处理标签被拆词的情况（"(bell" + "chimes)"）
    let joined: Vec<(u64, u64, String)> = {
        let mut v = Vec::new();
        let mut acc = String::new();
        let mut start = words.first().map(|w| w.start_ms).unwrap_or(0);
        for w in words {
            let t = w.text.trim();
            if t.is_empty() {
                continue;
            }
            if acc.is_empty() {
                start = w.start_ms;
            }
            acc.push_str(t);
            acc.push(' ');
            v.push((start, w.end_ms, acc.clone()));
        }
        v
    };
    for (start, _end, text) in &joined {
        let lower = text.to_ascii_lowercase();
        let _ = &lower;
        if let Some(re) = &re_material {
            if let Some(c) = re.captures(text) {
                push_unique(
                    &mut out,
                    Anchor {
                        time_ms: *start,
                        kind: AnchorKind::SpokenMaterial,
                        capture: c.get(2).map(|m| m.as_str().to_string()),
                    },
                );
            }
        }
        for re in [&re_question, &re_cn_question].into_iter().flatten() {
            if let Some(c) = re.captures(text) {
                push_unique(
                    &mut out,
                    Anchor {
                        time_ms: *start,
                        kind: AnchorKind::SpokenQuestion,
                        capture: c.get(1).map(|m| m.as_str().to_string()),
                    },
                );
            }
        }
    }
    out.sort_by_key(|a| a.time_ms);
    out
}

fn push_unique(out: &mut Vec<Anchor>, a: Anchor) {
    if out.last().map(|l| l.kind == a.kind && a.time_ms.saturating_sub(l.time_ms) < 1500).unwrap_or(false)
    {
        return;
    }
    out.push(a);
}

/// 空白长词段（whisper 把嘀声/间隔标成空文本的长段）—— 实测的可靠标记信号。
pub fn blank_spans(words: &[Word], min_ms: u64) -> Vec<(u64, u64)> {
    words
        .iter()
        .filter(|w| w.text.trim().is_empty() && w.end_ms.saturating_sub(w.start_ms) >= min_ms)
        .map(|w| (w.start_ms, w.end_ms))
        .collect()
}
