//! 库与缓存：SQLite（库/进度/分析缓存/生词本）。schema 见 docs/PLAN.md §5。

use lime_core::{normalize_chapters, Chapter, ChapterLevel, ChapterSource, Error, Media, Result, Sentence, Word};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaItem {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub duration_ms: u64,
    pub pos_ms: u64,
    pub chapter_ordinal: u32,
    pub is_analyzed: bool,
    pub plays: u32,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(|e| Error::Store(e.to_string()))?;
        let s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| Error::Store(e.to_string()))?;
        let s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(
                r#"
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS setting(k TEXT PRIMARY KEY, v TEXT);
                CREATE TABLE IF NOT EXISTS media(
                    id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, size INTEGER, mtime INTEGER,
                    duration_ms INTEGER, sample_rate INTEGER, channels INTEGER, added_at INTEGER);
                CREATE TABLE IF NOT EXISTS analysis(
                    media_id INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
                    model TEXT, params_hash TEXT, words_json_path TEXT, created_at INTEGER, version INTEGER);
                CREATE TABLE IF NOT EXISTS chapter(
                    id INTEGER PRIMARY KEY, media_id INTEGER REFERENCES media(id) ON DELETE CASCADE,
                    level INTEGER NOT NULL, parent_id INTEGER, ordinal INTEGER,
                    start_ms INTEGER, end_ms INTEGER, title TEXT, source TEXT,
                    confidence REAL, locked INTEGER DEFAULT 0, seq INTEGER);
                CREATE INDEX IF NOT EXISTS idx_chapter_media ON chapter(media_id, start_ms);
                CREATE TABLE IF NOT EXISTS sentence(
                    id INTEGER PRIMARY KEY,
                    media_id INTEGER REFERENCES media(id) ON DELETE CASCADE,
                    chapter_id INTEGER,
                    ordinal INTEGER,
                    start_ms INTEGER,
                    end_ms INTEGER,
                    text TEXT);
                CREATE INDEX IF NOT EXISTS idx_sentence_media ON sentence(media_id, start_ms);
                CREATE TABLE IF NOT EXISTS word(
                    id INTEGER PRIMARY KEY,
                    sentence_id INTEGER REFERENCES sentence(id) ON DELETE CASCADE,
                    media_id INTEGER REFERENCES media(id) ON DELETE CASCADE,
                    ordinal INTEGER,
                    start_ms INTEGER,
                    end_ms INTEGER,
                    text TEXT);
                CREATE INDEX IF NOT EXISTS idx_word_sentence ON word(sentence_id);
                CREATE INDEX IF NOT EXISTS idx_word_media ON word(media_id, start_ms);
                CREATE TABLE IF NOT EXISTS progress(
                    media_id INTEGER PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
                    chapter_id INTEGER, pos_ms INTEGER, updated_at INTEGER);
                CREATE TABLE IF NOT EXISTS listen_stat(
                    media_id INTEGER, chapter_id INTEGER, plays INTEGER DEFAULT 0,
                    loops INTEGER DEFAULT 0, last_at INTEGER, PRIMARY KEY(media_id, chapter_id));
                CREATE TABLE IF NOT EXISTS vocab(
                    id INTEGER PRIMARY KEY, word TEXT, lemma TEXT, media_id INTEGER,
                    chapter_id INTEGER, sentence_id INTEGER, start_ms INTEGER, end_ms INTEGER,
                    clip_path TEXT, note TEXT, created_at INTEGER);
                "#,
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        self.migrate_chapter_seq()?;
        self.conn
            .execute(
                "INSERT INTO setting(k,v) VALUES('schema_version', ?1)
                 ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// V2 → V3 迁移：章节表增加 `seq`（规范顺序）。
    ///
    /// 老库当初就是按「材料 → 其题」顺序逐条 INSERT 的，所以 rowid 顺序就是正确顺序，
    /// 直接按 rowid 回填即可让 `ORDER BY seq` 与旧的内存顺序完全一致——
    /// 这正是「左侧题目 UI 错乱」的根因（旧代码按 `ordinal` 排序，把材料全挤到前面、
    /// 把所有材料的第 1 题排在一起）。
    fn migrate_chapter_seq(&self) -> Result<()> {
        if !self.has_column("chapter", "seq")? {
            self.conn
                .execute_batch(
                    "ALTER TABLE chapter ADD COLUMN seq INTEGER;
                     UPDATE chapter SET seq = (
                         SELECT COUNT(*) FROM chapter c2
                         WHERE c2.media_id = chapter.media_id AND c2.id < chapter.id);",
                )
                .map_err(|e| Error::Store(e.to_string()))?;
        }
        self.conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_chapter_media_seq ON chapter(media_id, seq);")
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt.query([]).map_err(|e| Error::Store(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| Error::Store(e.to_string()))? {
            let name: String = row.get(1).map_err(|e| Error::Store(e.to_string()))?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 新增或更新媒体，返回 media_id。
    pub fn upsert_media(
        &self,
        path: &str,
        size: i64,
        mtime: i64,
        duration_ms: i64,
        sample_rate: i64,
        channels: i64,
    ) -> Result<i64> {
        let now = now_ms();
        self.conn
            .execute(
                "INSERT INTO media(path,size,mtime,duration_ms,sample_rate,channels,added_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(path) DO UPDATE SET size=excluded.size, mtime=excluded.mtime,
                   duration_ms=excluded.duration_ms, sample_rate=excluded.sample_rate, channels=excluded.channels",
                params![path, size, mtime, duration_ms, sample_rate, channels, now],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        self.conn
            .query_row("SELECT id FROM media WHERE path=?1", params![path], |r| r.get(0))
            .map_err(|e| Error::Store(e.to_string()))
    }

    pub fn get_media_by_path(&self, path: &str) -> Result<Option<Media>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, duration_ms, sample_rate, channels FROM media WHERE path=?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![path], |r| {
                Ok(Media {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    duration_ms: r.get::<_, i64>(2)? as u64,
                    sample_rate: r.get::<_, i64>(3)? as u32,
                    channels: r.get::<_, i64>(4)? as u16,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        match rows.next() {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(Error::Store(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn list_media(&self) -> Result<Vec<MediaItem>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.id, m.path, m.duration_ms,
                        COALESCE(p.pos_ms, 0), COALESCE(p.chapter_id, 0),
                        (SELECT COUNT(*) FROM chapter c WHERE c.media_id = m.id),
                        COALESCE((SELECT SUM(plays) FROM listen_stat s WHERE s.media_id = m.id), 0)
                 FROM media m
                 LEFT JOIN progress p ON p.media_id = m.id
                 ORDER BY m.added_at DESC",
            )
            .map_err(|e| Error::Store(e.to_string()))?;

        let rows = stmt
            .query_map([], |r| {
                let path: String = r.get(1)?;
                let filename = Path::new(&path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());
                let chapter_count: i64 = r.get(5)?;
                Ok(MediaItem {
                    id: r.get(0)?,
                    path,
                    filename,
                    duration_ms: r.get::<_, i64>(2)? as u64,
                    pos_ms: r.get::<_, i64>(3)? as u64,
                    chapter_ordinal: r.get::<_, i64>(4)? as u32,
                    is_analyzed: chapter_count > 0,
                    plays: r.get::<_, i64>(6)? as u32,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))
    }

    /// 全量替换（AI 切分结果 / `.limed` 缓存导入）。
    ///
    /// 约定：`chapters` 必须是「材料 → 其题」的规范顺序（`seq` = 下标，见
    /// `lime_core::normalize_chapters`）。写库时顺序、父下标、序号全按它落盘，
    /// 保证内存列表与库里读出来的顺序严格一致——题级微调用 `seq` 定位才不会打偏。
    ///
    /// 人工锁定（`locked=1`）的章节**保留人工修正的时间**，但会被放到本次分析的
    /// 规范位置（只更新 `seq`/`parent_id`），这样既不会丢人工修正，也不会留下
    /// 重复/错位的行；匹配不上的人工章节按原顺序追加到末尾。
    pub fn replace_chapters(&self, media_id: i64, chapters: &[Chapter]) -> Result<()> {
        self.write_chapters(media_id, chapters, false)
    }

    /// 全量覆盖（含人工锁定行）：拆分/合并等人工作业后的权威列表。
    pub fn overwrite_chapters(&self, media_id: i64, chapters: &[Chapter]) -> Result<()> {
        self.write_chapters(media_id, chapters, true)
    }

    fn write_chapters(&self, media_id: i64, chapters: &[Chapter], force: bool) -> Result<()> {
        let chapters = normalize_chapters(chapters);
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| Error::Store(e.to_string()))?;
        if force {
            tx.execute("DELETE FROM chapter WHERE media_id=?1", params![media_id])
                .map_err(|e| Error::Store(e.to_string()))?;
        } else {
            tx.execute("DELETE FROM chapter WHERE media_id=?1 AND locked=0", params![media_id])
                .map_err(|e| Error::Store(e.to_string()))?;
        }

        // 仍需保留的人工修正：按 (level, 父下标, 同级序号) 认领本次分析里的同名章节
        let mut kept: Vec<(i64, i64, i64, i64)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, level, COALESCE(parent_id, 0), COALESCE(ordinal, 0)
                     FROM chapter WHERE media_id=?1 AND locked=1 ORDER BY id",
                )
                .map_err(|e| Error::Store(e.to_string()))?;
            let rows = stmt
                .query_map(params![media_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(|e| Error::Store(e.to_string()))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Store(e.to_string()))?
        };

        for (i, c) in chapters.iter().enumerate() {
            let seq = i as i64;
            let level = if c.is_material() { 0i64 } else { 1i64 };
            let parent = c.parent.map(|p| p as i64 + 1).unwrap_or(0);
            let parent_val: Option<i64> = if parent > 0 { Some(parent) } else { None };

            let claim = kept.iter().position(|k| k.1 == level && k.2 == parent && k.3 == c.ordinal as i64);
            if let Some(pos) = claim {
                let id = kept.remove(pos).0;
                tx.execute(
                    "UPDATE chapter SET seq=?1, parent_id=?2 WHERE id=?3",
                    params![seq, parent_val, id],
                )
                .map_err(|e| Error::Store(e.to_string()))?;
                continue;
            }

            let source = match c.source {
                ChapterSource::Structure => "struct",
                ChapterSource::Asr => "asr",
                ChapterSource::Manual => "manual",
            };
            tx.execute(
                "INSERT INTO chapter(media_id,level,parent_id,ordinal,start_ms,end_ms,title,source,confidence,locked,seq)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    media_id,
                    level,
                    parent_val,
                    c.ordinal as i64,
                    c.start_ms as i64,
                    c.end_ms as i64,
                    c.title,
                    source,
                    c.confidence as f64,
                    c.locked as i64,
                    seq
                ],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }

        // 无法归位的人工章节：追加到末尾，保持 `seq` 唯一且顺序稳定
        let tail = chapters.len() as i64;
        for (k, item) in kept.iter().enumerate() {
            tx.execute(
                "UPDATE chapter SET seq=?1 WHERE id=?2",
                params![tail + k as i64, item.0],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }

        tx.commit().map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// 读取章节列表，**保证「材料 → 其题」规范顺序**：
    /// 按 `seq` 排序（没写 seq 的老行退回 rowid），把 `parent_id`（规范下标 + 1）
    /// 重新映射成实际下标，最后过一遍 `normalize_chapters` 兜底。
    pub fn load_chapters(&self, media_id: i64) -> Result<Vec<Chapter>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT level,parent_id,ordinal,start_ms,end_ms,title,source,confidence,locked,seq,id
                 FROM chapter WHERE media_id=?1
                 ORDER BY (seq IS NULL), seq, id",
            )
            .map_err(|e| Error::Store(e.to_string()))?;

        #[allow(clippy::type_complexity)]
        let mut raw: Vec<(Chapter, Option<u32>, i64)> = stmt
            .query_map(params![media_id], |r| {
                let level: i64 = r.get(0)?;
                let parent: Option<i64> = r.get(1)?;
                let seq: Option<i64> = r.get(9)?;
                Ok((
                    Chapter {
                        seq: seq.unwrap_or(0).max(0) as u32,
                        level: if level == 0 { ChapterLevel::Material } else { ChapterLevel::Question },
                        parent: parent.map(|p| (p - 1).max(0) as usize),
                        ordinal: r.get::<_, i64>(2)? as u32,
                        start_ms: r.get::<_, i64>(3)? as u64,
                        end_ms: r.get::<_, i64>(4)? as u64,
                        title: r.get(5)?,
                        source: match r.get::<_, String>(6)?.as_str() {
                            "asr" => ChapterSource::Asr,
                            "manual" => ChapterSource::Manual,
                            _ => ChapterSource::Structure,
                        },
                        confidence: r.get::<_, f64>(7)? as f32,
                        locked: r.get::<_, i64>(8)? != 0,
                    },
                    seq.map(|s| s.max(0) as u32),
                    r.get(10)?,
                ))
            })
            .map_err(|e| Error::Store(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))?;

        // 老库没回填到 seq（异常情形）：退回 rowid（当初的插入顺序）
        if raw.iter().any(|(_, seq, _)| seq.is_none()) {
            raw.sort_by_key(|(_, _, id)| *id);
        }

        let mut index_of: HashMap<u32, usize> = HashMap::new();
        for (i, (_, seq, _)) in raw.iter().enumerate() {
            index_of.entry(seq.unwrap_or(i as u32)).or_insert(i);
        }

        let mut out: Vec<Chapter> = Vec::with_capacity(raw.len());
        for (i, (mut c, seq, _)) in raw.into_iter().enumerate() {
            c.seq = seq.unwrap_or(i as u32);
            c.parent = c.parent.and_then(|p| index_of.get(&(p as u32)).copied());
            out.push(c);
        }
        Ok(normalize_chapters(&out))
    }

    pub fn save_sentences(&self, media_id: i64, sentences: &[Sentence]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM sentence WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM word WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;

        for (s_idx, s) in sentences.iter().enumerate() {
            let chapter_id = s.chapter.map(|c| c as i64);
            tx.execute(
                "INSERT INTO sentence(media_id, chapter_id, ordinal, start_ms, end_ms, text)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![media_id, chapter_id, s_idx as i64, s.start_ms as i64, s.end_ms as i64, s.text],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
            let sent_id = tx.last_insert_rowid();

            for (w_idx, w) in s.words.iter().enumerate() {
                tx.execute(
                    "INSERT INTO word(sentence_id, media_id, ordinal, start_ms, end_ms, text)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![sent_id, media_id, w_idx as i64, w.start_ms as i64, w.end_ms as i64, w.text],
                )
                .map_err(|e| Error::Store(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// 清空某音频的字幕/词缓存（**章节保留**）。
    ///
    /// 转写管线指纹变化时调用：旧参数（如 `-mc -1` 的重复幻觉）产生的字幕
    /// 不应再展示，等用户用新管线重新转写。
    pub fn clear_sentences(&self, media_id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM sentence WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        tx.execute("DELETE FROM word WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        tx.commit().map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    pub fn load_sentences(&self, media_id: i64) -> Result<Vec<Sentence>> {
        let mut s_stmt = self
            .conn
            .prepare(
                "SELECT id, chapter_id, start_ms, end_ms, text
                 FROM sentence WHERE media_id=?1 ORDER BY ordinal, start_ms",
            )
            .map_err(|e| Error::Store(e.to_string()))?;

        let mut w_stmt = self
            .conn
            .prepare(
                "SELECT sentence_id, start_ms, end_ms, text
                 FROM word WHERE media_id=?1 ORDER BY sentence_id, ordinal, start_ms",
            )
            .map_err(|e| Error::Store(e.to_string()))?;

        let mut words_map: std::collections::HashMap<i64, Vec<Word>> = std::collections::HashMap::new();
        let w_rows = w_stmt
            .query_map(params![media_id], |r| {
                let sid: i64 = r.get(0)?;
                let w = Word {
                    start_ms: r.get::<_, i64>(1)? as u64,
                    end_ms: r.get::<_, i64>(2)? as u64,
                    text: r.get(3)?,
                };
                Ok((sid, w))
            })
            .map_err(|e| Error::Store(e.to_string()))?;

        for row in w_rows {
            if let Ok((sid, w)) = row {
                words_map.entry(sid).or_default().push(w);
            }
        }

        let s_rows = s_stmt
            .query_map(params![media_id], |r| {
                let id: i64 = r.get(0)?;
                let chapter: Option<i64> = r.get(1)?;
                let start_ms = r.get::<_, i64>(2)? as u64;
                let end_ms = r.get::<_, i64>(3)? as u64;
                let text: String = r.get(4)?;
                let words = words_map.remove(&id).unwrap_or_default();
                Ok(Sentence {
                    start_ms,
                    end_ms,
                    text,
                    words,
                    chapter: chapter.map(|c| c as usize),
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;

        s_rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))
    }

    pub fn save_analysis(
        &self,
        media_id: i64,
        model: &str,
        params_hash: &str,
        words_json_path: &str,
    ) -> Result<()> {
        let now = now_ms();
        self.conn
            .execute(
                "INSERT INTO analysis(media_id, model, params_hash, words_json_path, created_at, version)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(media_id) DO UPDATE SET model=excluded.model,
                   params_hash=excluded.params_hash, words_json_path=excluded.words_json_path,
                   created_at=excluded.created_at, version=excluded.version",
                params![media_id, model, params_hash, words_json_path, now, SCHEMA_VERSION],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// 读取分析元信息 `(model, params_hash)`；没有记录时返回 `None`。
    ///
    /// `params_hash` 存的是转写管线指纹（`lime_asr::pipeline_fingerprint`）：
    /// 打开文件时用它判断旧字幕缓存是否还能用。
    pub fn load_analysis(&self, media_id: i64) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT model, params_hash FROM analysis WHERE media_id=?1",
                params![media_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                },
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))
    }

    pub fn has_analysis(&self, media_id: i64) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM chapter WHERE media_id=?1",
                params![media_id],
                |r| r.get(0),
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(count > 0)
    }

    pub fn save_progress(&self, media_id: i64, pos_ms: u64, chapter_ordinal: u32) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO progress(media_id,chapter_id,pos_ms,updated_at) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(media_id) DO UPDATE SET chapter_id=excluded.chapter_id,
                   pos_ms=excluded.pos_ms, updated_at=excluded.updated_at",
                params![media_id, chapter_ordinal as i64, pos_ms as i64, now_ms()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    pub fn get_progress(&self, media_id: i64) -> Result<Option<(u64, u32)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT pos_ms, chapter_id FROM progress WHERE media_id=?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![media_id], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u32))
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        match rows.next() {
            Some(Ok(p)) => Ok(Some(p)),
            Some(Err(e)) => Err(Error::Store(e.to_string())),
            None => Ok(None),
        }
    }

    /// 题级微调。按规范顺序 `seq` 定位：`ordinal` 在材料与题之间会撞号
    /// （每份材料的第 1 题都是 1），拿它当主键会一次改到别的材料的题。
    pub fn update_chapter_range(
        &self,
        media_id: i64,
        seq: u32,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE chapter SET start_ms=?1, end_ms=?2, source='manual', locked=1
                 WHERE media_id=?3 AND seq=?4",
                params![start_ms as i64, end_ms as i64, media_id, seq as i64],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// 在 `split_ms` 处把第 `seq` 章拆成两段（人工锁定，重跑分析不覆盖）。
    /// 返回是否真的拆开了（光标不在章节内部时不动）。
    pub fn split_chapter(&self, media_id: i64, seq: u32, split_ms: u64) -> Result<bool> {
        let chapters = self.load_chapters(media_id)?;
        let Some(idx) = chapters.iter().position(|c| c.seq == seq) else {
            return Ok(false);
        };
        let cur = chapters[idx].clone();
        if split_ms <= cur.start_ms || split_ms >= cur.end_ms {
            return Ok(false);
        }

        let mut updated: Vec<Chapter> = chapters[..idx].to_vec();
        let mut first = cur.clone();
        first.end_ms = split_ms;
        first.source = ChapterSource::Manual;
        first.locked = true;
        updated.push(first);
        let mut second = cur;
        second.start_ms = split_ms;
        second.source = ChapterSource::Manual;
        second.locked = true;
        updated.push(second);
        updated.extend_from_slice(&chapters[idx + 1..]);

        // 重排规范顺序：seq 连续、题号重算、后半段仍归属同一材料
        let normalized = normalize_chapters(&updated);
        self.overwrite_chapters(media_id, &normalized)?;
        Ok(true)
    }

    /// 把第 `seq` 章与**下一个同级兄弟**合并（题并题、材料并材料）。
    /// 返回是否真的合并了：不改跨材料吞并（最后一题不会吃掉下一份材料）。
    pub fn merge_next_chapter(&self, media_id: i64, seq: u32) -> Result<bool> {
        let chapters = self.load_chapters(media_id)?;
        let Some(idx) = chapters.iter().position(|c| c.seq == seq) else {
            return Ok(false);
        };
        let cur = &chapters[idx];
        let Some(next) = chapters.get(idx + 1) else {
            return Ok(false);
        };
        if next.level != cur.level || next.parent != cur.parent {
            return Ok(false);
        }

        let mut updated: Vec<Chapter> = Vec::with_capacity(chapters.len() - 1);
        for (i, c) in chapters.iter().enumerate() {
            if i == idx + 1 {
                continue;
            }
            let mut c = c.clone();
            if i == idx {
                c.end_ms = next.end_ms;
                c.source = ChapterSource::Manual;
                c.locked = true;
            }
            updated.push(c);
        }

        let normalized = normalize_chapters(&updated);
        self.overwrite_chapters(media_id, &normalized)?;
        Ok(true)
    }

    pub fn bump_stat(&self, media_id: i64, chapter_id: i64, loops: bool) -> Result<()> {
        let sql = if loops {
            "INSERT INTO listen_stat(media_id,chapter_id,plays,loops,last_at) VALUES(?1,?2,0,1,?3)
             ON CONFLICT(media_id,chapter_id) DO UPDATE SET loops=loops+1, last_at=excluded.last_at"
        } else {
            "INSERT INTO listen_stat(media_id,chapter_id,plays,loops,last_at) VALUES(?1,?2,1,0,?3)
             ON CONFLICT(media_id,chapter_id) DO UPDATE SET plays=plays+1, last_at=excluded.last_at"
        };
        self.conn
            .execute(sql, params![media_id, chapter_id, now_ms()])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(seq: u32, ordinal: u32, start: u64, end: u64, title: &str) -> Chapter {
        Chapter {
            seq,
            level: ChapterLevel::Material,
            parent: None,
            ordinal,
            start_ms: start,
            end_ms: end,
            title: title.into(),
            source: ChapterSource::Structure,
            confidence: 0.95,
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
            confidence: 0.9,
            locked: false,
        }
    }

    fn titles(chapters: &[Chapter]) -> Vec<String> {
        chapters.iter().map(|c| c.title.clone()).collect()
    }

    /// 两份材料、每份两道题（题号会重复），这是真实素材的典型形状。
    fn two_materials() -> Vec<Chapter> {
        vec![
            mat(0, 1, 0, 60_000, "材料 1"),
            question(1, 1, Some(0), 0, 20_000),
            question(2, 2, Some(0), 20_000, 60_000),
            mat(3, 2, 70_000, 130_000, "材料 2"),
            question(4, 1, Some(3), 70_000, 90_000),
            question(5, 2, Some(3), 90_000, 130_000),
        ]
    }

    fn open_with_media() -> (Store, i64) {
        let s = Store::open_in_memory().unwrap();
        let mid = s.upsert_media("test.mp3", 1000, 2000, 60_000, 44100, 2).unwrap();
        (s, mid)
    }

    #[test]
    fn test_store_crud() {
        let (s, mid) = open_with_media();
        assert!(mid > 0);

        let ch = two_materials();
        s.replace_chapters(mid, &ch).unwrap();
        let loaded_ch = s.load_chapters(mid).unwrap();
        assert_eq!(loaded_ch.len(), 6);

        // Sentences & words
        let sents = vec![
            Sentence {
                start_ms: 1000,
                end_ms: 5000,
                text: "Hello world".into(),
                words: vec![
                    Word { start_ms: 1000, end_ms: 2500, text: "Hello".into() },
                    Word { start_ms: 2600, end_ms: 5000, text: "world".into() },
                ],
                chapter: Some(0),
            },
        ];
        s.save_sentences(mid, &sents).unwrap();
        let loaded_s = s.load_sentences(mid).unwrap();
        assert_eq!(loaded_s.len(), 1);
        assert_eq!(loaded_s[0].words.len(), 2);
        assert_eq!(loaded_s[0].words[0].text, "Hello");

        // Progress
        s.save_progress(mid, 3500, 1).unwrap();
        let prog = s.get_progress(mid).unwrap();
        assert_eq!(prog, Some((3500, 1)));
    }

    /// 转写指纹与字幕清空：参数变化后旧字幕要能被丢弃（章节保留）。
    #[test]
    fn analysis_fingerprint_and_clear_sentences() {
        let (s, mid) = open_with_media();
        assert_eq!(s.load_analysis(mid).unwrap(), None);

        s.save_analysis(mid, "ggml-large-v3-turbo-q5_0.bin", "fp-old", "").unwrap();
        assert_eq!(
            s.load_analysis(mid).unwrap(),
            Some((
                "ggml-large-v3-turbo-q5_0.bin".to_string(),
                "fp-old".to_string()
            ))
        );
        // 同一 media 重复保存是覆盖写
        s.save_analysis(mid, "ggml-large-v3-turbo-q5_0.bin", "fp-new", "").unwrap();
        assert_eq!(s.load_analysis(mid).unwrap().unwrap().1, "fp-new");

        s.replace_chapters(mid, &two_materials()).unwrap();
        let sents = vec![Sentence {
            start_ms: 0,
            end_ms: 1000,
            text: "三个月之后,我会有十分钟的时间阅读一遍".into(),
            words: vec![Word { start_ms: 0, end_ms: 1000, text: "三个月之后".into() }],
            chapter: Some(0),
        }];
        s.save_sentences(mid, &sents).unwrap();
        assert_eq!(s.load_sentences(mid).unwrap().len(), 1);

        s.clear_sentences(mid).unwrap();
        assert!(s.load_sentences(mid).unwrap().is_empty());
        assert_eq!(s.load_chapters(mid).unwrap().len(), 6, "章节不能被顺带清掉");
    }

    /// 回归：左侧题目 UI 错乱的根因 —— 老代码 `ORDER BY ordinal, start_ms`
    /// 会把材料全挤到前面、把所有材料的「第 1 题」排在一起。
    #[test]
    fn chapters_keep_material_question_order() {
        let (s, mid) = open_with_media();
        s.replace_chapters(mid, &two_materials()).unwrap();

        let loaded = s.load_chapters(mid).unwrap();
        assert_eq!(
            titles(&loaded),
            vec!["引言", "第 1 题", "第 2 题", "材料 1", "第 1 题", "第 2 题"]
        );
        assert!(lime_core::chapters_are_canonical(&loaded));
        assert_eq!(loaded[1].parent, Some(0));
        assert_eq!(loaded[4].parent, Some(3));

        // 重新读一次（进程重启）：顺序、题号、父章节都不变
        let again = s.load_chapters(mid).unwrap();
        assert_eq!(again, loaded);
    }

    /// 老库（V2，没有 `seq`、行顺序是错乱的）打开时自动迁移归位。
    #[test]
    fn legacy_schema_is_migrated_and_regrouped() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media(id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, size INTEGER, mtime INTEGER,
                 duration_ms INTEGER, sample_rate INTEGER, channels INTEGER, added_at INTEGER);
             CREATE TABLE chapter(id INTEGER PRIMARY KEY, media_id INTEGER, level INTEGER NOT NULL, parent_id INTEGER,
                 ordinal INTEGER, start_ms INTEGER, end_ms INTEGER, title TEXT, source TEXT, confidence REAL,
                 locked INTEGER DEFAULT 0);
             INSERT INTO media(id,path,duration_ms) VALUES(1,'legacy.mp3',130000);",
        )
        .unwrap();

        // 老数据插入顺序 = 「材料全在前 + 全部第 1 题 + 全部第 2 题」，父下标 = 规范下标 + 1
        let rows: [(i64, i64, Option<i64>, i64, i64, &str); 6] = [
            (0, 0, None, 0, 60_000, "材料 1（叮咚）"),
            (0, 0, None, 70_000, 130_000, "材料 2（叮咚）"),
            (1, 1, Some(1), 0, 20_000, "第 1 题"),
            (1, 1, Some(2), 70_000, 90_000, "第 1 题"),
            (1, 2, Some(1), 20_000, 60_000, "第 2 题"),
            (1, 2, Some(2), 90_000, 130_000, "第 2 题"),
        ];
        for (level, ordinal, parent, start, end, title) in rows {
            conn.execute(
                "INSERT INTO chapter(media_id,level,parent_id,ordinal,start_ms,end_ms,title,source,confidence,locked)
                 VALUES(1,?1,?2,?3,?4,?5,?6,'struct',0.8,0)",
                params![level, parent, ordinal, start, end, title],
            )
            .unwrap();
        }

        let s = Store { conn };
        s.migrate().unwrap();
        let loaded = s.load_chapters(1).unwrap();
        assert_eq!(
            titles(&loaded),
            vec!["引言", "第 1 题", "第 2 题", "材料 1", "第 1 题", "第 2 题"]
        );
        assert!(lime_core::chapters_are_canonical(&loaded));
        assert_eq!(loaded[4].parent, Some(3));
        assert_eq!(loaded[4].start_ms, 70_000);
    }

    /// 题级微调按 `seq` 定位：媒体里有多个「第 1 题」时只能改中目标那一行。
    #[test]
    fn update_range_targets_single_chapter() {
        let (s, mid) = open_with_media();
        s.replace_chapters(mid, &two_materials()).unwrap();

        s.update_chapter_range(mid, 4, 69_000, 92_000).unwrap();
        let loaded = s.load_chapters(mid).unwrap();
        assert_eq!(loaded[4].start_ms, 69_000);
        assert_eq!(loaded[4].end_ms, 92_000);
        assert!(loaded[4].locked);
        assert_eq!(loaded[4].source, ChapterSource::Manual);

        // 另一份材料的「第 1 题」不受影响
        assert_eq!(loaded[1].start_ms, 0);
        assert_eq!(loaded[1].end_ms, 20_000);
        assert!(!loaded[1].locked);
    }

    /// 人工锁定后重跑分析：人工边界保留，其余跟随新结果，且不会出现重复/丢失。
    #[test]
    fn locked_chapter_survives_reanalysis() {
        let (s, mid) = open_with_media();
        let list = two_materials();
        s.replace_chapters(mid, &list).unwrap();
        s.update_chapter_range(mid, 4, 66_000, 92_000).unwrap();

        let mut reanalyzed = list.clone();
        reanalyzed[4].start_ms = 70_500;
        reanalyzed[4].end_ms = 90_500;
        reanalyzed[5].start_ms = 90_500;
        s.replace_chapters(mid, &reanalyzed).unwrap();

        let loaded = s.load_chapters(mid).unwrap();
        assert_eq!(loaded.len(), 6);
        assert_eq!(loaded[4].start_ms, 66_000, "人工修正必须保留");
        assert_eq!(loaded[4].end_ms, 92_000);
        assert_eq!(loaded[5].start_ms, 90_500, "其余章节跟随新分析");
        assert!(loaded[4].locked);
        assert!(lime_core::chapters_are_canonical(&loaded));
    }

    /// 拆分与合并：只在同一份材料内部动手，不会串到下一份材料。
    #[test]
    fn split_and_merge_stay_inside_material() {
        let (s, mid) = open_with_media();
        s.replace_chapters(mid, &two_materials()).unwrap();

        // 首份材料（引言）的第 1 题（seq = 1）在 8s 处拆成两题
        assert!(s.split_chapter(mid, 1, 8_000).unwrap());
        let loaded = s.load_chapters(mid).unwrap();
        assert_eq!(
            titles(&loaded),
            vec![
                "引言",
                "第 1 题",
                "第 2 题",
                "第 3 题",
                "材料 1",
                "第 1 题",
                "第 2 题"
            ]
        );
        assert_eq!(loaded[1].end_ms, 8_000);
        assert_eq!(loaded[2].start_ms, 8_000);
        assert_eq!(loaded[2].parent, Some(0));
        assert!(loaded[1].locked && loaded[2].locked);
        assert!(lime_core::chapters_are_canonical(&loaded));

        // 同级兄弟再合回来
        assert!(s.merge_next_chapter(mid, 1).unwrap());
        let merged = s.load_chapters(mid).unwrap();
        assert_eq!(merged.len(), 6);
        assert_eq!(merged[1].start_ms, 0);
        assert_eq!(merged[1].end_ms, 20_000);
        assert!(merged[1].locked);

        // 首份材料的最后一题没有同级下一题 → 不吃掉下一份材料
        assert!(!s.merge_next_chapter(mid, 2).unwrap());
        // 光标不在章节范围内 → 拆分不动
        assert!(!s.split_chapter(mid, 2, 61_000).unwrap());
        assert_eq!(s.load_chapters(mid).unwrap().len(), 6);
    }
}
