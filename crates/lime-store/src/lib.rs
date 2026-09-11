//! 库与缓存：SQLite（库/进度/分析缓存/生词本）。schema 见 docs/PLAN.md §5。

use lime_core::{Chapter, ChapterLevel, ChapterSource, Error, Result};
use rusqlite::{params, Connection};

pub const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
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

    pub fn replace_chapters(&self, media_id: i64, chapters: &[Chapter]) -> Result<()> {
        self.conn
            .execute("DELETE FROM chapter WHERE media_id=?1 AND locked=0", params![media_id])
            .map_err(|e| Error::Store(e.to_string()))?;
        for c in chapters {
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
                      FROM chapter WHERE media_id=?1 ORDER BY start_ms, level")
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
