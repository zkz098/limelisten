//! 库与缓存：SQLite（库/进度/分析缓存/生词本）。schema 见 docs/PLAN.md §5。

use lime_core::{Chapter, ChapterLevel, ChapterSource, Error, Media, Result, Sentence, Word};
use rusqlite::{params, Connection};
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 2;

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
                    confidence REAL, locked INTEGER DEFAULT 0);
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
        self.conn
            .execute(
                "INSERT INTO setting(k,v) VALUES('schema_version', ?1)
                 ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
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

    pub fn replace_chapters(&self, media_id: i64, chapters: &[Chapter]) -> Result<()> {
        self.conn
            .execute("DELETE FROM chapter WHERE media_id=?1 AND locked=0", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        for c in chapters {
            let locked_exists: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM chapter WHERE media_id=?1 AND ordinal=?2 AND locked=1",
                    params![media_id, c.ordinal as i64],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if locked_exists > 0 {
                continue;
            }

            let (level, parent_id) = match c.level {
                ChapterLevel::Material => (0i64, None),
                ChapterLevel::Question => (1i64, c.parent.map(|p| p as i64 + 1)),
            };
            let source = match c.source {
                ChapterSource::Structure => "struct",
                ChapterSource::Asr => "asr",
                ChapterSource::Manual => "manual",
            };
            self.conn
                .execute(
                    "INSERT INTO chapter(media_id,level,parent_id,ordinal,start_ms,end_ms,title,source,confidence,locked)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        media_id,
                        level,
                        parent_id,
                        c.ordinal as i64,
                        c.start_ms as i64,
                        c.end_ms as i64,
                        c.title,
                        source,
                        c.confidence as f64,
                        c.locked as i64
                    ],
                )
                .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    pub fn load_chapters(&self, media_id: i64) -> Result<Vec<Chapter>> {
        let mut stmt = self
            .conn
            .prepare("SELECT level,parent_id,ordinal,start_ms,end_ms,title,source,confidence,locked
                      FROM chapter WHERE media_id=?1 ORDER BY ordinal, start_ms")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![media_id], |r| {
                let level: i64 = r.get(0)?;
                let parent: Option<i64> = r.get(1)?;
                Ok(Chapter {
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
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))
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

    pub fn update_chapter_range(
        &self,
        media_id: i64,
        ordinal: u32,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE chapter SET start_ms=?1, end_ms=?2, source='manual', locked=1
                 WHERE media_id=?3 AND ordinal=?4",
                params![start_ms as i64, end_ms as i64, media_id, ordinal as i64],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    pub fn split_chapter(&self, media_id: i64, ordinal: u32, split_ms: u64) -> Result<()> {
        let chapters = self.load_chapters(media_id)?;
        let Some((idx, cur)) = chapters.iter().enumerate().find(|(_, c)| c.ordinal == ordinal) else {
            return Ok(());
        };
        if split_ms <= cur.start_ms || split_ms >= cur.end_ms {
            return Ok(());
        }

        let mut updated = Vec::new();
        for (i, c) in chapters.iter().enumerate() {
            if i < idx {
                updated.push(c.clone());
            } else if i == idx {
                let mut first = c.clone();
                first.end_ms = split_ms;
                first.source = ChapterSource::Manual;
                first.locked = true;
                updated.push(first);

                let mut second = c.clone();
                second.start_ms = split_ms;
                second.title = format!("{} (2)", c.title);
                second.ordinal = c.ordinal + 1;
                second.source = ChapterSource::Manual;
                second.locked = true;
                updated.push(second);
            } else {
                let mut shifted = c.clone();
                shifted.ordinal += 1;
                updated.push(shifted);
            }
        }
        self.conn
            .execute("DELETE FROM chapter WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        self.replace_chapters(media_id, &updated)?;
        Ok(())
    }

    pub fn merge_next_chapter(&self, media_id: i64, ordinal: u32) -> Result<()> {
        let chapters = self.load_chapters(media_id)?;
        let Some((idx, cur)) = chapters.iter().enumerate().find(|(_, c)| c.ordinal == ordinal) else {
            return Ok(());
        };
        let Some(next) = chapters.get(idx + 1) else {
            return Ok(());
        };

        let mut updated = Vec::new();
        for (i, c) in chapters.iter().enumerate() {
            if i < idx {
                updated.push(c.clone());
            } else if i == idx {
                let mut merged = cur.clone();
                merged.end_ms = next.end_ms;
                merged.source = ChapterSource::Manual;
                merged.locked = true;
                updated.push(merged);
            } else if i == idx + 1 {
                // skip next
            } else {
                let mut shifted = c.clone();
                shifted.ordinal = shifted.ordinal.saturating_sub(1);
                updated.push(shifted);
            }
        }
        self.conn
            .execute("DELETE FROM chapter WHERE media_id=?1", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        self.replace_chapters(media_id, &updated)?;
        Ok(())
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

    #[test]
    fn test_store_crud() {
        let s = Store::open_in_memory().unwrap();
        let mid = s.upsert_media("test.mp3", 1000, 2000, 60_000, 44100, 2).unwrap();
        assert!(mid > 0);

        let ch = vec![
            Chapter {
                level: ChapterLevel::Material,
                parent: None,
                ordinal: 1,
                start_ms: 1000,
                end_ms: 20_000,
                title: "Text 1".into(),
                source: ChapterSource::Structure,
                confidence: 0.95,
                locked: false,
            },
            Chapter {
                level: ChapterLevel::Question,
                parent: Some(0),
                ordinal: 2,
                start_ms: 2000,
                end_ms: 8_000,
                title: "Question 1".into(),
                source: ChapterSource::Structure,
                confidence: 0.9,
                locked: false,
            },
        ];
        s.replace_chapters(mid, &ch).unwrap();
        let loaded_ch = s.load_chapters(mid).unwrap();
        assert_eq!(loaded_ch.len(), 2);

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

        // Split & Merge
        s.split_chapter(mid, 1, 10_000).unwrap();
        let ch_split = s.load_chapters(mid).unwrap();
        assert_eq!(ch_split.len(), 3);
        assert_eq!(ch_split[0].end_ms, 10_000);
        assert_eq!(ch_split[1].start_ms, 10_000);

        s.merge_next_chapter(mid, 1).unwrap();
        let ch_merged = s.load_chapters(mid).unwrap();
        assert_eq!(ch_merged.len(), 2);
        assert_eq!(ch_merged[0].end_ms, 20_000);
    }
}
