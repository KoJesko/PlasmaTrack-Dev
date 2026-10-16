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
    /// Client notes placed among `frames`; marked uploaded with the chunk.
    pub note_ids: Vec<i64>,
    /// When the oldest of these frames arrived (the client's clock).
    pub oldest_ms: i64,
    /// It reached `max_frames` or `max_bytes`: more is waiting.
    pub full: bool,
    /// It carries a game event (a delivery, a cancellation), which is sent at once.
    pub urgent: bool,
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
-- Client-inserted frames (profile changes, PLAN §6b). They carry no plugin seq;
-- each belongs right after plugin frame `after_seq` of its session.
CREATE TABLE IF NOT EXISTS notes (
    id        INTEGER PRIMARY KEY,
    sess      TEXT    NOT NULL,
    after_seq INTEGER NOT NULL,
    json      TEXT    NOT NULL,
    uploaded  INTEGER NOT NULL DEFAULT 0,
    added_ms  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS notes_pending ON notes (uploaded, sess, after_seq);
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
        self.store_fresh(batch, now_ms).map(|(acks, _)| acks)
    }

    /// [`Db::store`], plus the indices of the frames that were new (a plugin
    /// replays unacknowledged frames after a reconnect).
    pub fn store_fresh(&mut self, batch: &[Incoming], now_ms: i64) -> Result<(BTreeMap<String, u64>, Vec<usize>)> {
        let mut acks: BTreeMap<String, u64> = BTreeMap::new();
        let mut fresh = Vec::new();
        let tx = self.conn.transaction()?;
        {
            let mut touch = tx.prepare_cached(
                "INSERT INTO sessions (sess, first_seen_ms, last_seen_ms) VALUES (?1, ?2, ?2)
                 ON CONFLICT(sess) DO UPDATE SET last_seen_ms = excluded.last_seen_ms",
            )?;
            let mut frame = tx.prepare_cached(
                "INSERT OR IGNORE INTO frames (sess, seq, kind, json, received_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (i, m) in batch.iter().enumerate() {
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
                        if frame.execute(params![env.sess, seq as i64, kind, m.raw, now_ms])? == 1 {
                            fresh.push(i);
                        }
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
        Ok((acks, fresh))
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
            "SELECT seq, json, kind, received_ms FROM frames WHERE sess = ?1 AND uploaded = 0 ORDER BY seq LIMIT ?2",
        )?;
        let mut rows = st.query(params![sess, max_frames as i64])?;
        let mut plugin: Vec<(u64, String)> = Vec::new();
        let mut bytes = 0usize;
        let (mut oldest_ms, mut urgent, mut full) = (i64::MAX, false, false);
        while let Some(r) = rows.next()? {
            let seq: i64 = r.get(0)?;
            let json: String = r.get(1)?;
            if !plugin.is_empty() && bytes + json.len() > max_bytes {
                full = true;
                break;
            }
            bytes += json.len();
            urgent |= r.get::<_, String>(2)? == "gameplay";
            oldest_ms = oldest_ms.min(r.get(3)?);
            plugin.push((seq as u64, json));
        }
        full |= plugin.len() >= max_frames;
        let (Some(&(from, _)), Some(&(to, _))) = (plugin.first(), plugin.last()) else {
            return Ok(None);
        };
        // Notes go right after the frame they followed (or, when that frame was
        // dropped by the plugin or already uploaded, before the next one).
        let mut st = self.conn.prepare_cached(
            "SELECT id, after_seq, json FROM notes WHERE sess = ?1 AND uploaded = 0 AND after_seq <= ?2 ORDER BY after_seq, id",
        )?;
        let notes: Vec<(i64, u64, String)> = st
            .query_map(params![sess, to as i64], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64, r.get(2)?)))?
            .collect::<Result<_>>()?;
        let mut frames = Vec::with_capacity(plugin.len() + notes.len());
        let mut note_ids = Vec::with_capacity(notes.len());
        let mut pending_notes = notes.into_iter().peekable();
        for (seq, json) in plugin {
            while let Some((id, _, note)) = pending_notes.next_if(|n| n.1 < seq) {
                note_ids.push(id);
                frames.push(note);
            }
            frames.push(json);
            while let Some((id, _, note)) = pending_notes.next_if(|n| n.1 == seq) {
                note_ids.push(id);
                frames.push(note);
            }
        }
        let hello: Option<String> = self
            .conn
            .query_row("SELECT hello FROM sessions WHERE sess = ?1", params![sess], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(Some(PendingChunk { sess: sess.to_owned(), seq_from: from, seq_to: to, frames, hello, note_ids, oldest_ms, full, urgent }))
    }

    /// Records a client note (e.g. a profile change) at the current end of
    /// `sess`: it follows the newest plugin frame stored so far. Returns its JSON.
    pub fn add_note(&mut self, sess: &str, mut note: serde_json::Value, now_ms: i64) -> Result<String> {
        let after: i64 =
            self.conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM frames WHERE sess = ?1", params![sess], |r| r.get(0))?;
        note["sess"] = sess.into();
        note["after_seq"] = after.into();
        let json = note.to_string();
        self.conn.execute("INSERT INTO notes (sess, after_seq, json, added_ms) VALUES (?1, ?2, ?3, ?4)", params![sess, after, json, now_ms])?;
        Ok(json)
    }

    pub fn mark_notes_uploaded(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in ids {
            tx.execute("UPDATE notes SET uploaded = 1 WHERE id = ?1", params![id])?;
        }
        tx.commit()
    }

    /// The newest session of a game (`game_id` as the plugin reports it) and when
    /// a frame of it last arrived.
    pub fn latest_session(&self, game_id: &str) -> Result<Option<(String, i64)>> {
        self.conn
            .query_row(
                "SELECT sess, last_seen_ms FROM sessions WHERE game_id = ?1 ORDER BY first_seen_ms DESC, sess DESC LIMIT 1",
                params![game_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// Sessions last seen at or after `since_ms`, oldest first, with their game.
    pub fn sessions_since(&self, since_ms: i64) -> Result<Vec<(String, Option<String>)>> {
        let mut st = self
            .conn
            .prepare("SELECT sess, game_id FROM sessions WHERE last_seen_ms >= ?1 ORDER BY first_seen_ms, sess")?;
        let rows = st.query_map(params![since_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Every frame and note of `sess` stored before `before_ms`, in stream order
    /// (for rebuilding the live timeline after a restart).
    pub fn session_stream(&self, sess: &str, before_ms: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT json FROM (
                 SELECT seq AS k, 0 AS n, json FROM frames WHERE sess = ?1 AND received_ms < ?2
                 UNION ALL
                 SELECT after_seq AS k, id AS n, json FROM notes WHERE sess = ?1 AND added_ms < ?2
             ) ORDER BY k, n",
        )?;
        let rows = st.query_map(params![sess, before_ms], |r| r.get(0))?;
        rows.collect()
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
    fn notes_ride_in_stream_order() {
        let mut db = Db::open_in_memory().unwrap();
        let note = |db: &mut Db, k: &str| db.add_note(S1, serde_json::json!({"v": 1, "kind": "profile", "profile_key": k}), 1).unwrap();
        let first = note(&mut db, "a"); // before any frame
        db.store(&[msg(S1, "init", Some(1), ",\"game_id\":\"ats\""), msg(S1, "sample", Some(2), "")], 1).unwrap();
        assert_eq!(db.latest_session("ats").unwrap(), Some((S1.to_string(), 1)));
        let second = note(&mut db, "b"); // after seq 2
        db.store(&[msg(S1, "sample", Some(4), ""), msg(S1, "sample", Some(5), "")], 2).unwrap();
        assert!(first.contains("\"after_seq\":0") && second.contains("\"after_seq\":2"));

        let c = db.next_chunk(S1, 100, usize::MAX).unwrap().unwrap();
        let kinds: Vec<String> = c
            .frames
            .iter()
            .map(|f| {
                let v: serde_json::Value = serde_json::from_str(f).unwrap();
                format!("{}{}", v["kind"].as_str().unwrap(), v["profile_key"].as_str().unwrap_or(""))
            })
            .collect();
        assert_eq!(kinds, ["profilea", "init", "sample", "profileb", "sample", "sample"]);
        assert_eq!(db.session_stream(S1, i64::MAX).unwrap(), c.frames, "a rebuild sees the same order");
        assert_eq!(db.session_stream(S1, 2).unwrap().len(), 4, "only what was stored before");
        assert_eq!(c.note_ids.len(), 2);

        db.mark_uploaded(S1, c.seq_to).unwrap();
        db.mark_notes_uploaded(&c.note_ids).unwrap();
        let third = note(&mut db, "c"); // after seq 5, already uploaded
        assert!(third.contains("\"after_seq\":5"));
        db.store(&[msg(S1, "sample", Some(6), "")], 3).unwrap();
        let c = db.next_chunk(S1, 100, usize::MAX).unwrap().unwrap();
        assert_eq!(c.frames.len(), 2, "the late note leads the next chunk");
        assert!(c.frames[0].contains("profile_key"));
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
