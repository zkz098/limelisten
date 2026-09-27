//! 领域模型：媒体、章节、句子、词、生词。无 IO 依赖。

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("asr: {0}")]
    Asr(String),
    #[error("store: {0}")]
    Store(String),
    #[error("limed: {0}")]
    Limed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub mod limed;
pub use limed::{find_accompanying_audio, find_matching_limed, LimedFile, LimedMeta};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    pub path: String,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// 两级切分：材料（Passage/Text）与题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterLevel {
    /// Passage / Text N —— 一整个对话或短文
    Material,
    /// 题号（按顺序推断，或来自念白正则）
    Question,
}

/// 章节来源：结构检测 / ASR 锚点 / 人工修正。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterSource {
    Structure,
    Asr,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    /// 规范顺序 = 在章节列表中的下标（0 起、连续）。
    ///
    /// 左侧「章节导航」是扁平列表 + `level` 缩进渲染的，`current` / 上一题 / 下一题
    /// 也都用列表下标定位，所以顺序一旦错乱，题就会挂到错误的材料下面。
    /// 任何进入 UI 的章节列表都应先过一遍 [`normalize_chapters`]；
    /// 数据库/`.limed` 里的 `seq` 与内存列表保持一致（见 `lime-store`）。
    #[serde(default)]
    pub seq: u32,
    pub level: ChapterLevel,
    /// 父章节在列表中的索引（Question -> Material），Material 为 None。
    pub parent: Option<usize>,
    /// 同级序号，从 1 开始（材料 = 第几份材料，题 = 材料内第几题）。
    pub ordinal: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
    pub source: ChapterSource,
    /// 0.0–1.0，低置信在 UI 标黄提示人工修正。
    pub confidence: f32,
    /// 人工锁定后重跑分析不覆盖。
    pub locked: bool,
}

impl Chapter {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn is_material(&self) -> bool {
        self.level == ChapterLevel::Material
    }
}

/// 首份材料的标题：听力文件开头通常是开考提示/试音/说明段，独立为「引言」，
/// 不参与材料编号（`材料 1` 从第二份材料起算）。
pub const INTRO_TITLE: &str = "引言";

/// 章节列表是否已是规范顺序：`seq` = 下标、材料在前、每道题紧跟其材料。
///
/// 仅用于测试与 debug 断言；不满足时用 [`normalize_chapters`] 整理。
pub fn chapters_are_canonical(chapters: &[Chapter]) -> bool {
    let mut current_parent: Option<usize> = None;
    for (i, c) in chapters.iter().enumerate() {
        if c.seq as usize != i {
            return false;
        }
        if c.is_material() {
            if c.parent.is_some() {
                return false;
            }
            current_parent = Some(i);
        } else if c.parent != current_parent {
            // 题必须紧跟同一份材料下的兄弟题，不能跨材料乱挂。
            return false;
        }
    }
    true
}

/// 把章节整理成「材料 → 其题」的规范顺序（幂等，可安全重复调用）。
///
/// 认父顺序：显式 `parent`（且与题的时间区间有交集）→ 时间包含 → 时间重叠 →
/// 最近的前一份材料。据此即使拿到顺序错乱、父下标失效的老数据
/// （例如老库 `ORDER BY ordinal` 读出的"材料全在前、题全在后"），
/// 也能把每道题放回它真正的材料下面，并重建题号与自动题名。
///
/// 无法归入任何材料的题（列表里根本没有材料）保持原有层级，追加在末尾。
pub fn normalize_chapters(chapters: &[Chapter]) -> Vec<Chapter> {
    if chapters.is_empty() {
        return Vec::new();
    }

    let order = canonical_order(chapters);
    let mats: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| chapters[i].is_material())
        .collect();

    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); chapters.len()];
    let mut loose: Vec<usize> = Vec::new();
    for &i in order.iter().filter(|&&i| !chapters[i].is_material()) {
        match pick_parent_material(chapters, i, &mats) {
            Some(m) => kids[m].push(i),
            None => loose.push(i),
        }
    }

    let mut out: Vec<Chapter> = Vec::with_capacity(chapters.len());
    // 首份材料若是自动命名的（`材料 N` / `材料 N（叮咚）`）或已是「引言」，
    // 则规范为「引言」：老缓存/老库里叫「材料 1」的开头段也能自动升级；
    // 后续材料从「材料 1」重新编号。人工/锚点改过名的首份材料保持原样。
    let first_is_intro = mats
        .first()
        .is_some_and(|&i| is_intro_or_auto_material(&chapters[i]));

    for (mi, &index) in mats.iter().enumerate() {
        let mat_pos = out.len() as usize;
        let seq = out.len() as u32;
        let mut mat = chapters[index].clone();
        mat.seq = seq;
        mat.level = ChapterLevel::Material;
        mat.parent = None;
        mat.ordinal = mi as u32 + 1;
        if first_is_intro && mi == 0 {
            mat.title = INTRO_TITLE.to_string();
        } else {
            // 引言不占材料号：它后面的材料显示为「材料 1」
            let display_ord = if first_is_intro { mi as u32 } else { mi as u32 + 1 };
            if let Some(t) = renumber_material_title(&mat.title, display_ord) {
                mat.title = t;
            }
        }
        out.push(mat);

        for (qi, &q_index) in kids[index].iter().enumerate() {
            let mut q = chapters[q_index].clone();
            q.seq = out.len() as u32;
            q.parent = Some(mat_pos);
            q.ordinal = qi as u32 + 1;
            if let Some(t) = renumber_question_title(&q.title, q.ordinal) {
                q.title = t;
            }
            out.push(q);
        }
    }

    for &index in &loose {
        let mut c = chapters[index].clone();
        c.seq = out.len() as u32;
        c.parent = None;
        out.push(c);
    }

    out
}

/// 取规范顺序的下标序列：`seq` 唯一就按 `seq`，否则（老 `.limed` 里全是 0）按原顺序。
fn canonical_order(chapters: &[Chapter]) -> Vec<usize> {
    let n = chapters.len();
    let mut sorted: Vec<u32> = chapters.iter().map(|c| c.seq).collect();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != n {
        return (0..n).collect();
    }
    if chapters.windows(2).all(|w| w[0].seq < w[1].seq) {
        return (0..n).collect();
    }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by_key(|&i| chapters[i].seq);
    idx
}

/// 给一道题认亲（返回材料在**输入列表**中的下标）。
fn pick_parent_material(chapters: &[Chapter], qi: usize, mats: &[usize]) -> Option<usize> {
    if mats.is_empty() {
        return None;
    }
    let q = &chapters[qi];
    let q_end = q.end_ms.max(q.start_ms + 1);
    let overlaps = |m: usize| {
        let p = &chapters[m];
        p.start_ms < q_end && p.end_ms > q.start_ms
    };

    // 1) 显式父链接：只要还落在某份材料的时间范围内就认（人工微调过的题可能越界，
    //    但完全不相交说明父下标已经失效，例如顺序被打乱过）。
    if let Some(p) = q.parent {
        if mats.contains(&p) && overlaps(p) {
            return Some(p);
        }
    }

    // 2) 题中点落在材料区间内
    let mid = q.start_ms + q.duration_ms() / 2;
    if let Some(&m) = mats
        .iter()
        .find(|&&m| mid >= chapters[m].start_ms && mid < chapters[m].end_ms)
    {
        return Some(m);
    }

    // 3) 与材料区间相交
    if let Some(&m) = mats.iter().find(|&&m| overlaps(m)) {
        return Some(m);
    }

    // 4) 最近的前一份材料（否则最早的一份）
    let mut by_start = mats.to_vec();
    by_start.sort_by_key(|&m| (chapters[m].start_ms, m));
    by_start
        .iter()
        .rev()
        .copied()
        .find(|&m| chapters[m].start_ms <= q.start_ms)
        .or_else(|| by_start.first().copied())
}

/// 判断首份材料是否应当按「引言」处理：已经是引言，或名字仍是自动生成的 `材料 N`。
fn is_intro_or_auto_material(c: &Chapter) -> bool {
    c.title.trim() == INTRO_TITLE || renumber_material_title(&c.title, 0).is_some()
}

/// 自动题名形如「第 3 题」，可带历史遗留的「(2)」后缀；人工改名返回 None 不动。
fn renumber_question_title(title: &str, ordinal: u32) -> Option<String> {
    let rest = strip_ordinal_suffix(title).strip_prefix("第")?.trim();
    let rest = rest.strip_suffix("题")?.trim();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("第 {ordinal} 题"))
}

/// 自动材料名形如「材料 4」/「材料 4（叮咚）」；人工改名（`第一节`、`Text 1`…）不动。
/// 重新编号时剥离历史残留的「（叮咚）」，统一规范为「材料 N」。
fn renumber_material_title(title: &str, ordinal: u32) -> Option<String> {
    const MARK: &str = "（叮咚）";
    let t = title.trim();
    let head = match t.strip_suffix(MARK) {
        Some(h) => h,
        None => t,
    };
    let rest = head.trim_end().strip_prefix("材料")?.trim();
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("材料 {ordinal}"))
}

/// 去掉「改 (2)」这类历史遗留的序号后缀。
fn strip_ordinal_suffix(title: &str) -> &str {
    let s = title.trim();
    for (open, close) in [('(', ')'), ('（', '）')] {
        if let Some(rest) = s.strip_suffix(close) {
            if let Some(pos) = rest.rfind(open) {
                let inner = &rest[pos + open.len_utf8()..];
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                    return rest[..pos].trim_end();
                }
            }
        }
    }
    s
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sentence {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<Word>,
    /// 所属题章节索引（0-based，指向章节列表）。
    pub chapter: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VocabEntry {
    pub word: String,
    pub lemma: String,
    pub media_id: i64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub sentence: String,
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(seq: u32, ordinal: u32, start: u64, end: u64) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Material,
            parent: None,
            ordinal,
            start_ms: start,
            end_ms: end,
            title: format!("材料 {ordinal}"),
            source: ChapterSource::Structure,
            confidence: 0.9,
            locked: false,
        }
    }

    fn question(seq: u32, ordinal: u32, parent: Option<usize>, start: u64, end: u64) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Question,
            parent,
            ordinal,
            start_ms: start,
            end_ms: end,
            title: format!("第 {ordinal} 题"),
            source: ChapterSource::Structure,
            confidence: 0.8,
            locked: false,
        }
    }

    fn titles(chapters: &[Chapter]) -> Vec<String> {
        chapters.iter().map(|c| c.title.clone()).collect()
    }

    #[test]
    fn normalize_is_idempotent_on_canonical_list() {
        let mut m1 = mat(0, 1, 0, 60_000);
        m1.title = INTRO_TITLE.into();
        let mut m2 = mat(3, 2, 70_000, 130_000);
        m2.title = "材料 1".into(); // 引言不占材料号
        let list = vec![
            m1,
            question(1, 1, Some(0), 0, 20_000),
            question(2, 2, Some(0), 20_000, 60_000),
            m2,
            question(4, 1, Some(3), 70_000, 90_000),
        ];
        assert!(chapters_are_canonical(&list));
        let out = normalize_chapters(&list);
        assert_eq!(out, list);
        assert!(chapters_are_canonical(&out));
    }

    /// 复现用户截图里的错乱：老库 `ORDER BY ordinal, start_ms` 读出的顺序
    /// （材料全部在前，然后是全部「第 1 题」，再是全部「第 2 题」）。
    #[test]
    fn normalize_regroups_materials_first_layout() {
        let m1 = mat(0, 1, 0, 60_000);
        let m2 = mat(1, 2, 70_000, 130_000);
        let a1 = question(2, 1, Some(0), 0, 20_000);
        let a2 = question(3, 2, Some(0), 20_000, 60_000);
        let b1 = question(4, 1, Some(1), 70_000, 130_000);
        let broken = vec![m1, m2, a1, b1, a2];

        let fixed = normalize_chapters(&broken);
        assert_eq!(
            titles(&fixed),
            vec!["引言", "第 1 题", "第 2 题", "材料 1", "第 1 题"]
        );
        assert!(chapters_are_canonical(&fixed));
        assert_eq!(fixed[1].parent, Some(0));
        assert_eq!(fixed[2].parent, Some(0));
        assert_eq!(fixed[4].parent, Some(3));
        assert_eq!(fixed[2].start_ms, 20_000);
    }

    /// 父下标失效（指向了别的材料）时按时间重新认父。
    #[test]
    fn normalize_repairs_stale_parent_index() {
        let m1 = mat(0, 1, 0, 60_000);
        let m2 = mat(1, 2, 70_000, 130_000);
        let mut b1 = question(2, 1, Some(0), 70_000, 130_000);
        b1.title = "第 1 题".into();
        let out = normalize_chapters(&[m1, m2, b1]);
        assert_eq!(titles(&out), vec!["引言", "材料 1", "第 1 题"]);
        assert_eq!(out[2].parent, Some(1));
    }

    #[test]
    fn normalize_rebuilds_ordinals_and_auto_titles() {
        let m = mat(0, 0, 0, 100_000);
        let mut q1 = question(1, 1, Some(0), 0, 40_000);
        q1.title = "第 1 题 (2)".into();
        let mut q2 = question(2, 3, Some(0), 40_000, 100_000);
        q2.title = "第 3 题".into();
        let mut named = question(3, 9, Some(0), 50_000, 80_000);
        named.title = "第二问（用户改名）".into();

        let out = normalize_chapters(&[m, q1, q2, named]);
        assert_eq!(
            titles(&out),
            vec!["引言", "第 1 题", "第 2 题", "第二问（用户改名）"]
        );
        assert_eq!(out[1].ordinal, 1);
        assert_eq!(out[2].ordinal, 2);
    }

    /// 首份材料（开考提示/试音）规范为「引言」，后续材料从「材料 1」顺延编号；幂等。
    #[test]
    fn first_material_becomes_intro_and_others_renumber() {
        let m1 = mat(0, 1, 0, 60_000);
        let m2 = mat(1, 2, 70_000, 130_000);
        let q1 = question(2, 1, Some(0), 0, 20_000);
        let q2 = question(3, 1, Some(1), 70_000, 130_000);
        let out = normalize_chapters(&[m1, m2, q1, q2]);
        assert_eq!(titles(&out), vec!["引言", "第 1 题", "材料 1", "第 1 题"]);
        assert!(chapters_are_canonical(&out));
        assert_eq!(normalize_chapters(&out), out, "引言规范化必须幂等");

        // 老缓存里的 `材料 1（叮咚）` 也应自动升级为「引言」
        let mut old = mat(0, 1, 0, 60_000);
        old.title = "材料 1（叮咚）".into();
        let out = normalize_chapters(&[old, mat(1, 2, 70_000, 130_000)]);
        assert_eq!(titles(&out), vec!["引言", "材料 1"]);
    }

    /// 人工/锚点改过名的首份材料不被强制改成「引言」，后续材料照常编号。
    #[test]
    fn custom_first_material_title_is_kept() {
        let mut m1 = mat(0, 1, 0, 60_000);
        m1.title = "Text 1".into();
        let m2 = mat(1, 2, 70_000, 130_000);
        let out = normalize_chapters(&[m1, m2]);
        assert_eq!(titles(&out), vec!["Text 1", "材料 2"]);
    }

    #[test]
    fn normalize_without_materials_keeps_questions() {
        let q = question(0, 1, None, 0, 10_000);
        let out = normalize_chapters(&[q.clone()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, ChapterLevel::Question);
        assert_eq!(out[0].parent, None);
    }

    #[test]
    fn legacy_limed_seq_zeros_keep_input_order() {
        // 老 .limed 没有 seq 字段，serde 会填 0；此时按输入顺序处理。
        let m = mat(0, 0, 0, 60_000);
        let q = question(0, 0, Some(0), 0, 60_000);
        let out = normalize_chapters(&[m, q]);
        assert!(chapters_are_canonical(&out));
        assert_eq!(out[1].parent, Some(0));
    }
}
