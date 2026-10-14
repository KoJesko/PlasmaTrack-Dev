//! Profile detection from the game's own log (PLAN §6b). The SDK has no profile
//! identifier, but every world load writes
//!
//! ```text
//! 00:00:36.178 : Loading save. Type: 0, slot: 0, path: <MEMFILE><UFS>/home/profiles/<HEX>/save/autosave/game.sii
//! ```
//!
//! to `Documents/<game>/game.log.txt` (captured in ATS and ETS2, 2026-09-28 and
//! 2026-10-13). `<HEX>` is the profile name in hexadecimal and `home` / `steam`
//! says whether it is a local or a Steam Cloud profile. Only a hash of both
//! leaves this PC; the profile name never does.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use pt_core::Game;
use sha2::{Digest, Sha256};

pub const LOG_FILE: &str = "game.log.txt";

/// The profile key a `Loading save` line names, if it is one.
pub fn profile_key_from_line(line: &str) -> Option<String> {
    let rest = &line[line.find("Loading save.")?..];
    let path = &rest[rest.find("path:")? + 5..];
    let at = path.find("/profiles/")?;
    let location = path[..at].rsplit('/').next().unwrap_or_default();
    let tail = &path[at + "/profiles/".len()..];
    let hex = &tail[..tail.find('/')?];
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(profile_key(location, hex))
}

/// 16 hex digits of SHA-256 over `"{location}/{HEX}"`: stable per profile,
/// says nothing about its name.
pub fn profile_key(location: &str, hex: &str) -> String {
    let digest = Sha256::digest(format!("{}/{}", location.to_ascii_lowercase(), hex.to_ascii_uppercase()));
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The game's documents folder name.
pub fn documents_folder(game: Game) -> &'static str {
    match game {
        Game::Ets2 => "Euro Truck Simulator 2",
        Game::Ats => "American Truck Simulator",
    }
}

/// Where each game writes its log: the Windows documents folder, and under
/// Proton the prefix of every Steam library that has the game.
pub fn log_paths(game: Game, steam_libraries: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        if let Some(docs) = dirs::document_dir() {
            out.push(docs.join(documents_folder(game)).join(LOG_FILE));
        }
    } else {
        let app_id = match game {
            Game::Ets2 => "227300",
            Game::Ats => "270880",
        };
        for lib in steam_libraries {
            out.push(
                lib.join("steamapps/compatdata")
                    .join(app_id)
                    .join("pfx/drive_c/users/steamuser/Documents")
                    .join(documents_folder(game))
                    .join(LOG_FILE),
            );
        }
    }
    out
}

/// Follows a log file that the game rewrites at every start. The first line
/// ("log created on : <date>") tells a new log from the old one even when the
/// new one is already longer.
pub struct LogTail {
    path: PathBuf,
    pos: u64,
    partial: Vec<u8>,
    head: Vec<u8>,
    rewrites: u64,
}

const HEAD_BYTES: u64 = 80;

impl LogTail {
    /// Starts at the beginning, so the profile loaded before the client started
    /// is found too.
    pub fn new(path: PathBuf) -> LogTail {
        LogTail { path, pos: 0, partial: Vec::new(), head: Vec::new(), rewrites: 0 }
    }

    /// How often the game has rewritten the log (a new game start) so far.
    pub fn rewrites(&self) -> u64 {
        self.rewrites
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Complete lines written since the last call.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(mut f) = File::open(&self.path) else { return Vec::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let mut head = Vec::new();
        if (&mut f).take(HEAD_BYTES).read_to_end(&mut head).is_err() {
            return Vec::new();
        }
        let same_head = head.starts_with(&self.head[..self.head.len().min(head.len())]);
        if len < self.pos || !same_head {
            // A new game start rewrote the file.
            self.pos = 0;
            self.partial.clear();
            self.rewrites += 1;
        }
        self.head = head;
        if len == self.pos || f.seek(SeekFrom::Start(self.pos)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if f.take(len - self.pos).read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        self.pos += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let mut lines = Vec::new();
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            lines.push(String::from_utf8_lossy(&line).trim_end().to_string());
        }
        lines
    }
}

/// How recently a session must have sent a frame to count as running.
const ACTIVE_WALL_MS: i64 = 30_000;

/// Per game: which profile the log names, and the profile note last written.
struct Watched {
    game: Game,
    tails: Vec<LogTail>,
    /// Rewrites already seen, per tail.
    rewrites: Vec<u64>,
    wanted: Option<String>,
    sent: Option<(String, String)>,
}

/// Follows both games' logs and records a `profile` note in the running session
/// of that game whenever a save of a (possibly other) profile is loaded. A note
/// waits until the game's session is running; a fresh log (new game start)
/// forgets the previous profile.
pub async fn watch(
    db: std::sync::Arc<std::sync::Mutex<crate::db::Db>>,
    feed: Option<crate::live::Feed>,
    steam_libraries: Vec<PathBuf>,
) {
    let mut games: Vec<Watched> = [Game::Ets2, Game::Ats]
        .into_iter()
        .map(|game| Watched {
            game,
            tails: log_paths(game, &steam_libraries).into_iter().map(LogTail::new).collect(),
            rewrites: Vec::new(),
            wanted: None,
            sent: None,
        })
        .collect();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        for w in games.iter_mut() {
            w.rewrites.resize(w.tails.len(), 0);
            for (t, seen) in w.tails.iter_mut().zip(w.rewrites.iter_mut()) {
                let lines = t.poll();
                if t.rewrites() != *seen {
                    *seen = t.rewrites();
                    w.wanted = None;
                }
                if let Some(key) = lines.iter().rev().find_map(|l| profile_key_from_line(l)) {
                    w.wanted = Some(key);
                }
            }
            let Some(key) = w.wanted.clone() else { continue };
            let now = crate::now_ms();
            let db2 = db.clone();
            let game_id = w.game.sdk_game_id();
            let sent = w.sent.clone();
            let note = tokio::task::spawn_blocking(move || -> rusqlite::Result<Option<(String, String)>> {
                let mut db = db2.lock().expect("db");
                let Some((sess, last_seen)) = db.latest_session(game_id)? else { return Ok(None) };
                if now - last_seen > ACTIVE_WALL_MS || sent.as_ref() == Some(&(sess.clone(), key.clone())) {
                    return Ok(None);
                }
                let json = db.add_note(&sess, serde_json::json!({"v": 1, "kind": "profile", "wall_ms": now, "profile_key": key}), now)?;
                Ok(Some((sess, json)))
            })
            .await;
            match note {
                Ok(Ok(Some((sess, json)))) => {
                    crate::log!("{:?} profile {} in session {sess}", w.game, w.wanted.as_deref().unwrap_or_default());
                    w.sent = Some((sess, w.wanted.clone().unwrap_or_default()));
                    if let Some(f) = &feed {
                        f.send(now, vec![json]);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => crate::log!("cannot record the profile: {e}"),
                Err(e) => crate::log!("profile task failed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "00:00:36.178 : Loading save. Type: 0, slot: 0, path: <MEMFILE><UFS>/home/profiles/4E616D65/save/autosave/game.sii";
    const STEAM: &str = "01:07:20.930 : Loading save. Type: 0, slot: 0, path: <MEMFILE><UFS>/steam/profiles/4E616D65/save/autosave/game.sii";

    #[test]
    fn keys_come_from_loading_save_lines_only() {
        let home = profile_key_from_line(HOME).unwrap();
        assert_eq!(home.len(), 16);
        assert_eq!(profile_key_from_line(&HOME.replace("/save/autosave", "/save/461")), Some(home.clone()));
        assert_ne!(profile_key_from_line(STEAM).unwrap(), home, "local and Steam Cloud profiles differ");
        assert_eq!(profile_key_from_line(&HOME.replace("4E616D65", "4e616d65")), Some(home), "hex case does not matter");
        assert_eq!(profile_key_from_line("00:00:15.928 : New profile selected: 'Name'"), None);
        assert_eq!(profile_key_from_line("Loading save. path: /home/profiles/not-hex/save/x"), None);
    }

    #[test]
    fn tail_follows_appends_and_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        std::fs::write(&path, "a\nb").unwrap();
        let mut t = LogTail::new(path.clone());
        assert_eq!(t.poll(), vec!["a"]);
        std::fs::write(&path, "a\nbc\r\nd\n").unwrap();
        assert_eq!(t.poll(), vec!["bc", "d"]);
        assert!(t.poll().is_empty());
        std::fs::write(&path, "new\n").unwrap();
        assert_eq!(t.poll(), vec!["new"], "the game rewrote the log");
        std::fs::write(&path, "NEW\nmuch longer than before\n").unwrap();
        assert_eq!(t.poll(), vec!["NEW", "much longer than before"], "a longer rewrite is noticed too");
    }
}
