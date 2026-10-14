//! `POST /api/v1/chunks` (docs/UPLOAD_API.md). Stores what is new in a chunk
//! and answers at once; the player's scorer picks it up afterwards.
//!
//! The client sends a session's frames strictly in seq order and resends a
//! chunk until it is acknowledged, so the new part of a chunk is: plugin frames
//! above the highest seq stored, and client notes not stored before. Stored
//! chunks of a session therefore never overlap and replay in `chunk_no` order.

use pt_core::upload::{UploadAck, UploadChunk, UPLOAD_PROTOCOL};
use pt_core::Game;
use serde_json::{json, Value};

use crate::env::{Host, RowExt};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::params;
use crate::{auth, store, util};

/// Compressed request body.
pub const MAX_BODY_BYTES: usize = 16 << 20;
/// After decompression.
pub const MAX_JSON_BYTES: usize = 96 << 20;
/// Client notes remembered per session for de-duplication.
const NOTES_REMEMBERED: usize = 500;

pub fn raw_key(user_id: &str, sess: &str, chunk_no: i64) -> String {
    format!("raw/{user_id}/{sess}/{chunk_no:06}.jsonl.gz")
}

fn note_identity(v: &Value) -> String {
    format!("{}:{}:{}", v["after_seq"], v["wall_ms"], v["profile_key"].as_str().unwrap_or_default())
}

pub async fn upload<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let user = auth::player(h, req).await?;
    if req.body.len() > MAX_BODY_BYTES {
        return Err(ApiError::too_large(MAX_BODY_BYTES));
    }
    let gzip = req.header("content-encoding").is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let json = if gzip {
        util::gunzip(&req.body, MAX_JSON_BYTES).map_err(|e| ApiError::bad_request(&format!("bad gzip body: {e}")))?
    } else {
        req.body.clone()
    };
    let chunk: UploadChunk = serde_json::from_slice(&json).map_err(|e| ApiError::bad_request(&format!("bad chunk: {e}")))?;
    if chunk.protocol != UPLOAD_PROTOCOL {
        return Err(ApiError::bad_request("unsupported upload protocol").details(json!({"protocol": chunk.protocol})));
    }
    if chunk.sess.len() != 32 || !chunk.sess.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("sess must be 32 hex digits"));
    }
    let now = h.now_ms();
    let session = store::one(h, "SELECT * FROM plugin_sessions WHERE user_id = ?1 AND sess = ?2", params![&user.user_id, &chunk.sess])
        .await?;
    let stored_through = session.as_ref().map_or(0, |s| s.i("stored_through")) as u64;
    let mut notes_seen: Vec<String> =
        session.as_ref().and_then(|s| serde_json::from_value(s.json("notes_seen")).ok()).unwrap_or_default();

    // What is new, in order.
    let mut fresh: Vec<&str> = Vec::new();
    let (mut max_seq, mut game, mut first_wall) = (stored_through, None::<Game>, None::<i64>);
    let mut tmp: Vec<(u64, i64)> = Vec::new();
    for raw in &chunk.frames {
        let v: Value = serde_json::from_str(raw.get()).map_err(|e| ApiError::bad_request(&format!("bad frame: {e}")))?;
        let wall = v["wall_ms"].as_i64().unwrap_or(0);
        if wall > 0 && first_wall.is_none_or(|w| wall < w) {
            first_wall = Some(wall);
        }
        match v["seq"].as_u64() {
            Some(seq) => {
                if seq <= stored_through {
                    continue;
                }
                max_seq = max_seq.max(seq);
            }
            None => {
                let id = note_identity(&v);
                if v["kind"] != "profile" || notes_seen.contains(&id) {
                    continue;
                }
                notes_seen.push(id);
            }
        }
        match v["kind"].as_str() {
            Some("init") => game = v["game_id"].as_str().and_then(Game::from_sdk_game_id).or(game),
            Some("truckersmp") if v["connected"] == true => {
                if let Some(id) = v["account_id"].as_u64().filter(|id| *id > 0) {
                    tmp.push((id, wall));
                }
            }
            _ => {}
        }
        fresh.push(raw.get());
    }
    let accepted_through = max_seq.max(chunk.seq_to);
    let ack = Response::ok(serde_json::to_value(UploadAck { sess: chunk.sess.clone(), accepted_through }).expect("ack"));
    if fresh.is_empty() {
        return Ok(ack);
    }
    if notes_seen.len() > NOTES_REMEMBERED {
        notes_seen.drain(..notes_seen.len() - NOTES_REMEMBERED);
    }

    let chunk_no = session.as_ref().map_or(0, |s| s.i("chunk_count"));
    let key = raw_key(&user.user_id, &chunk.sess, chunk_no);
    let mut jsonl = Vec::with_capacity(fresh.iter().map(|f| f.len() + 1).sum());
    for f in &fresh {
        jsonl.extend_from_slice(f.as_bytes());
        jsonl.push(b'\n');
    }
    let blob = util::gzip(&jsonl);
    let bytes = blob.len() as i64;
    h.blob_put(&key, blob).await?;

    let game_s = game.map(|g| g.as_str().to_string());
    let hello = chunk.hello.as_ref().map(|r| r.get().to_string());
    let manifest = serde_json::to_string(&chunk.manifest).expect("manifest");
    let first_wall = first_wall.unwrap_or(now);
    let mut stmts = vec![
        (
            "INSERT INTO plugin_sessions (user_id, sess, game, first_wall_ms, received_at, hello, client_version,
                 config_hash, config_json, manifest, stored_through, chunk_count, notes_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12)
             ON CONFLICT (user_id, sess) DO UPDATE SET
                 game = COALESCE(plugin_sessions.game, excluded.game),
                 first_wall_ms = MIN(plugin_sessions.first_wall_ms, excluded.first_wall_ms),
                 hello = COALESCE(excluded.hello, plugin_sessions.hello),
                 client_version = excluded.client_version, config_hash = excluded.config_hash,
                 config_json = excluded.config_json, manifest = excluded.manifest,
                 stored_through = excluded.stored_through,
                 chunk_count = plugin_sessions.chunk_count + 1, notes_seen = excluded.notes_seen"
                .to_string(),
            params![
                &user.user_id,
                &chunk.sess,
                game_s.clone(),
                first_wall,
                now,
                hello,
                &chunk.client_version,
                &chunk.config_hash,
                chunk.config.get(),
                manifest,
                max_seq,
                serde_json::to_string(&notes_seen).expect("notes")
            ],
        ),
        (
            "INSERT OR REPLACE INTO raw_chunks (user_id, sess, chunk_no, seq_from, seq_to, frames, bytes, r2_key, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                .to_string(),
            params![&user.user_id, &chunk.sess, chunk_no, chunk.seq_from, max_seq, fresh.len() as i64, bytes, &key, now],
        ),
    ];
    for (account, wall) in tmp {
        let at = if wall > 0 { wall } else { now };
        stmts.push((
            "INSERT INTO truckersmp_accounts (user_id, account_id, first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT (user_id, account_id) DO UPDATE SET
                 first_seen_at = MIN(truckersmp_accounts.first_seen_at, excluded.first_seen_at),
                 last_seen_at = MAX(truckersmp_accounts.last_seen_at, excluded.last_seen_at)"
                .to_string(),
            params![&user.user_id, account, at],
        ));
    }
    h.batch(stmts).await?;

    let game = match game_s {
        Some(g) => Some(g),
        None => session.as_ref().and_then(|s| s.opt_s("game")),
    };
    if let Some(g) = game {
        h.poke_scorer(&user.user_id, &g).await?;
    }
    Ok(ack)
}
