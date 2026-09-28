//! The client's local SQLite log. Frames are stored verbatim, keyed by
//! (session, seq), so replays from the plugin are idempotent. A frame is only
//! acknowledged to the plugin after the transaction holding it has committed
//! (`synchronous=FULL`), so an acknowledged frame survives a crash of the client.

use std::collections::BTreeMap;
use std::path::Path;

use pt_core::wire::Envelope;
use rusqlite::{params, Connection, OptionalExtension};

pub type Result<T> = rusqlite::Result<T>;

/// A frame received from the plugin, already parsed for its envelope.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub env: Envelope,
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub sess: String,
    pub seq_from: u64,
    pub seq_to: u64,
    pub frames: Vec<String>,
    pub hello: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DbStats {
    pub sessions: u64,
    pub frames: u64,
    pub frames_pending_upload: u64,
    pub snapshots: u64,
}

pub struct Db {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    sess          TEXT PRIMARY KEY,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms  INTEGER NOT NULL,
    game_id       TEXT,
    hello         TEXT
);
CREATE TABLE IF NOT EXISTS frames (
    sess        TEXT    NOT NULL,
    seq         INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    json        TEXT    NOT NULL,
    received_ms INTEGER NOT NULL,
    uploaded    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (sess, seq)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS frames_pending ON frames (uploaded, sess, seq);
CREATE TABLE IF NOT EXISTS snapshots (
    id          INTEGER PRIMARY KEY,
    sess        TEXT    NOT NULL,
    received_ms INTEGER NOT NULL,
    json        TEXT    NOT NULL
);
CREATE TABLE IF NOT EXISTS uploads (
    id       INTEGER PRIMARY KEY,
    at_ms    INTEGER NOT NULL,
    sess     TEXT    NOT NULL,
    seq_from INTEGER NOT NULL,
    seq_to   INTEGER NOT NULL,
    ok       INTEGER NOT NULL,
    detail   TEXT    NOT NULL
);
";

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    /// Persists a batch in one transaction and returns, per session, the highest
    /// seq now stored. The caller acknowledges exactly those to the plugin.
    pub fn store(&mut self, batch: &[Incoming], now_ms: i64) -> Result<BTreeMap<String, u64>> {
        let mut acks: BTreeMap<String, u64> = BTreeMap::new();
        let tx = self.conn.transaction()?;
        {
            let mut touch = tx.prepare_cached(
                "INSERT INTO sessions (sess, first_seen_ms, last_seen_ms) VALUES (?1, ?2, ?2)
                 ON CONFLICT(sess) DO UPDATE SET last_seen_ms = excluded.last_seen_ms",
            )?;
            let mut frame = tx.prepare_cached(
                "INSERT OR IGNORE INTO frames (sess, seq, kind, json, received_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for m in batch {
                let env = &m.env;
                touch.execute(params![env.sess, now_ms])?;
                match (env.kind.as_str(), env.seq) {
                    ("hello", _) => {
                        tx.execute("UPDATE sessions SET hello = ?2 WHERE sess = ?1", params![env.sess, m.raw])?;
                    }
                    ("snapshot", _) => {
                        tx.execute(
                            "INSERT INTO snapshots (sess, received_ms, json) VALUES (?1, ?2, ?3)",
                            params![env.sess, now_ms, m.raw],
                        )?;
                    }
                    (kind, Some(seq)) => {
                        frame.execute(params![env.sess, seq as i64, kind, m.raw, now_ms])?;
                        if kind == "init" {
                            if let Some(game) = serde_json::from_str::<serde_json::Value>(&m.raw)
                                .ok()
                                .and_then(|v| v.get("game_id").and_then(|g| g.as_str()).map(str::to_owned))
                            {
                                tx.execute("UPDATE sessions SET game_id = ?2 WHERE sess = ?1", params![env.sess, game])?;
                            }
                        }
                        let e = acks.entry(env.sess.clone()).or_insert(0);
                        *e = (*e).max(seq);
                    }
                    (_, None) => {} // unknown seq-less kind: nothing to persist
                }
            }
        }
        tx.commit()?;
        Ok(acks)
    }

    /// Sessions that still have frames to upload, oldest first.
    pub fn pending_sessions(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT s.sess FROM sessions s
             WHERE EXISTS (SELECT 1 FROM frames f WHERE f.uploaded = 0 AND f.sess = s.sess)
             ORDER BY s.first_seen_ms, s.sess",
        )?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    /// The next run of unuploaded frames of `sess`, in seq order, bounded by
    /// `max_frames` and (approximately) `max_bytes`.
    pub fn next_chunk(&self, sess: &str, max_frames: usize, max_bytes: usize) -> Result<Option<PendingChunk>> {
        let mut st = self.conn.prepare_cached(
            "SELECT seq, json FROM frames WHERE sess = ?1 AND uploaded = 0 ORDER BY seq LIMIT ?2",
        )?;
        let mut rows = st.query(params![sess, max_frames as i64])?;
        let mut frames = Vec::new();
        let (mut from, mut to, mut bytes) = (0u64, 0u64, 0usize);
        while let Some(r) = rows.next()? {
            let seq: i64 = r.get(0)?;
            let json: String = r.get(1)?;
            if !frames.is_empty() && bytes + json.len() > max_bytes {
                break;
            }
            if frames.is_empty() {
                from = seq as u64;
            }
            to = seq as u64;
            bytes += json.len();
            frames.push(json);
        }
        if frames.is_empty() {
            return Ok(None);
        }
        let hello: Option<String> = self
            .conn
            .query_row("SELECT hello FROM sessions WHERE sess = ?1", params![sess], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(Some(PendingChunk { sess: sess.to_owned(), seq_from: from, seq_to: to, frames, hello }))
    }

    /// Marks every frame of `sess` up to `through_seq` as accepted by the server.
    pub fn mark_uploaded(&mut self, sess: &str, through_seq: u64) -> Result<usize> {
        self.conn.execute(
            "UPDATE frames SET uploaded = 1 WHERE sess = ?1 AND seq <= ?2 AND uploaded = 0",
            params![sess, through_seq as i64],
        )
    }

    pub fn record_upload(&mut self, sess: &str, from: u64, to: u64, ok: bool, detail: &str, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO uploads (at_ms, sess, seq_from, seq_to, ok, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now_ms, sess, from as i64, to as i64, ok, detail],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Result<DbStats> {
        let count = |sql: &str| -> Result<u64> { self.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|v| v as u64) };
        Ok(DbStats {
            sessions: count("SELECT COUNT(*) FROM sessions")?,
            frames: count("SELECT COUNT(*) FROM frames")?,
            frames_pending_upload: count("SELECT COUNT(*) FROM frames WHERE uploaded = 0")?,
            snapshots: count("SELECT COUNT(*) FROM snapshots")?,
        })
    }

    pub fn frame_seqs(&self, sess: &str) -> Result<Vec<u64>> {
        let mut st = self.conn.prepare("SELECT seq FROM frames WHERE sess = ?1 ORDER BY seq")?;
        let rows = st.query_map(params![sess], |r| r.get::<_, i64>(0).map(|v| v as u64))?;
        rows.collect()
    }

    pub fn session_game(&self, sess: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT game_id FROM sessions WHERE sess = ?1", params![sess], |r| r.get(0))
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S1: &str = "11111111111111111111111111111111";
    const S2: &str = "22222222222222222222222222222222";

    fn msg(sess: &str, kind: &str, seq: Option<u64>, extra: &str) -> Incoming {
        let raw = match seq {
            Some(s) => format!("{{\"v\":1,\"kind\":\"{kind}\",\"sess\":\"{sess}\",\"seq\":{s}{extra}}}"),
            None => format!("{{\"v\":1,\"kind\":\"{kind}\",\"sess\":\"{sess}\"{extra}}}"),
        };
        Incoming { env: Envelope::parse(&raw).unwrap(), raw }
    }

    #[test]
    fn store_is_idempotent_and_acks_max_seq() {
        let mut db = Db::open_in_memory().unwrap();
        let batch = vec![
            msg(S1, "hello", None, ""),
            msg(S1, "snapshot", None, ",\"paused\":true"),
            msg(S1, "init", Some(1), ",\"game_id\":\"ats\""),
            msg(S1, "sample", Some(2), ""),
            msg(S1, "sample", Some(4), ""), // 3 was dropped by the plugin
        ];
        let acks = db.store(&batch, 10).unwrap();
        assert_eq!(acks.get(S1), Some(&4));
        // Replay after a reconnect: duplicates are ignored, new frames added.
        let acks = db.store(&[msg(S1, "sample", Some(2), ""), msg(S1, "sample", Some(5), "")], 20).unwrap();
        assert_eq!(acks.get(S1), Some(&5));
        assert_eq!(db.frame_seqs(S1).unwrap(), vec![1, 2, 4, 5]);
        assert_eq!(db.session_game(S1).unwrap().as_deref(), Some("ats"));
        let st = db.stats().unwrap();
        assert_eq!((st.sessions, st.frames, st.frames_pending_upload, st.snapshots), (1, 4, 4, 1));
    }

    #[test]
    fn chunks_respect_limits_and_upload_marks() {
        let mut db = Db::open_in_memory().unwrap();
        let mut batch = vec![msg(S1, "hello", None, "")];
        batch.extend((1..=10).map(|i| msg(S1, "sample", Some(i), "")));
        batch.extend((1..=3).map(|i| msg(S2, "config", Some(i), "")));
        db.store(&batch, 1).unwrap();
        assert_eq!(db.pending_sessions().unwrap().len(), 2);

        let c = db.next_chunk(S1, 4, usize::MAX).unwrap().unwrap();
        assert_eq!((c.seq_from, c.seq_to, c.frames.len()), (1, 4, 4));
        assert!(c.hello.is_some());
        let one_frame = c.frames[0].len();
        let c = db.next_chunk(S1, 100, one_frame * 2).unwrap().unwrap();
        assert_eq!(c.frames.len(), 2);

        assert_eq!(db.mark_uploaded(S1, 4).unwrap(), 4);
        let c = db.next_chunk(S1, 100, usize::MAX).unwrap().unwrap();
        assert_eq!((c.seq_from, c.seq_to), (5, 10));
        db.mark_uploaded(S1, 10).unwrap();
        assert!(db.next_chunk(S1, 100, usize::MAX).unwrap().is_none());
        assert_eq!(db.pending_sessions().unwrap(), vec![S2.to_string()]);
        assert!(db.next_chunk(S2, 100, usize::MAX).unwrap().unwrap().hello.is_none());
    }

    #[test]
    fn survives_reopen_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.sqlite");
        {
            let mut db = Db::open(&path).unwrap();
            db.store(&[msg(S1, "config", Some(7), "")], 1).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.frame_seqs(S1).unwrap(), vec![7]);
    }
}
