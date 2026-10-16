//! Uploads raw frames to the server (`POST /api/v1/chunks`). A failed upload is
//! retried at a fixed interval (`[sync] retry_interval_wall_s`, 20 s by default)
//! forever, until it succeeds; unsent frames stay queued in SQLite, so this
//! survives restarts of the client and the PC.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::write::GzEncoder;
use flate2::Compression;
use pt_core::upload::{ManifestEntry, UploadAck, UploadChunk, CHUNKS_PATH, UPLOAD_PROTOCOL};
use pt_core::wire::redact_modules;
use serde_json::value::RawValue;

use crate::db::{Db, PendingChunk};
use crate::log;
use crate::status::SharedStatus;

#[derive(Debug, Clone)]
pub struct UploaderConfig {
    pub server_url: String,
    pub token: String,
    pub retry_interval: Duration,
    /// How often to look for new frames when the queue is empty.
    pub idle_poll: Duration,
    /// Frames wait this long to be sent together, unless a chunk is full or
    /// carries a game event (owner, 2026-10-16: sending every few frames made
    /// 60-145 requests a minute per driver, which capped the server at about
    /// 50-100 drivers at once).
    pub batch_interval: Duration,
    pub max_frames: usize,
    pub max_bytes: usize,
    pub truckersmp_module_names: Vec<String>,
    pub config_hash: String,
    pub config_canonical_json: String,
}

impl UploaderConfig {
    pub fn is_configured(&self) -> bool {
        !self.token.trim().is_empty() && !self.server_url.contains(".invalid")
    }
}

/// Builds the request body for one pending chunk (modules redacted).
pub fn build_chunk(cfg: &UploaderConfig, pending: &PendingChunk, manifest: &[ManifestEntry]) -> Result<UploadChunk, String> {
    let raw = |s: String| RawValue::from_string(s).map_err(|e| e.to_string());
    Ok(UploadChunk {
        protocol: UPLOAD_PROTOCOL,
        client_version: crate::CLIENT_VERSION.to_string(),
        sess: pending.sess.clone(),
        hello: pending.hello.clone().map(raw).transpose()?,
        seq_from: pending.seq_from,
        seq_to: pending.seq_to,
        frames: pending
            .frames
            .iter()
            .map(|f| raw(redact_modules(f, &cfg.truckersmp_module_names)))
            .collect::<Result<_, _>>()?,
        config_hash: cfg.config_hash.clone(),
        config: raw(cfg.config_canonical_json.clone())?,
        manifest: manifest.to_vec(),
    })
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes).expect("in-memory write");
    enc.finish().expect("in-memory finish")
}

async fn post(client: &reqwest::Client, cfg: &UploaderConfig, chunk: &UploadChunk) -> Result<UploadAck, String> {
    let body = gzip(&serde_json::to_vec(chunk).map_err(|e| e.to_string())?);
    let url = format!("{}{}", cfg.server_url.trim_end_matches('/'), CHUNKS_PATH);
    let resp = client
        .post(url)
        .bearer_auth(cfg.token.trim())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let code = resp.status();
    let text = resp.text().await.map_err(|e| format!("reading response: {e}"))?;
    if !code.is_success() {
        return Err(format!("server returned {code}: {}", text.chars().take(200).collect::<String>()));
    }
    let ack: UploadAck = serde_json::from_str(&text).map_err(|e| format!("bad ack: {e}"))?;
    if ack.sess != chunk.sess || ack.accepted_through < chunk.seq_from {
        return Err(format!("ack does not cover the chunk: {ack:?}"));
    }
    Ok(ack)
}

/// Runs forever. `manifest` is refreshed by the caller when files change.
pub async fn run(cfg: UploaderConfig, db: Arc<Mutex<Db>>, status: SharedStatus, manifest: Arc<Mutex<Vec<ManifestEntry>>>) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("plasmatrack-client/{}", crate::CLIENT_VERSION))
        .build()
        .expect("HTTP client");
    status.lock().expect("status").upload_configured = cfg.is_configured();

    loop {
        let next = {
            let db = db.clone();
            let (max_frames, max_bytes) = (cfg.max_frames, cfg.max_bytes);
            tokio::task::spawn_blocking(move || -> rusqlite::Result<Option<PendingChunk>> {
                let db = db.lock().expect("db");
                for sess in db.pending_sessions()? {
                    if let Some(c) = db.next_chunk(&sess, max_frames, max_bytes)? {
                        return Ok(Some(c));
                    }
                }
                Ok(None)
            })
            .await
        };
        let pending = match next {
            Ok(Ok(Some(p))) if p.full || p.urgent => p,
            Ok(Ok(Some(p))) => {
                let wait = cfg.batch_interval.as_millis() as i64 - (crate::now_ms() - p.oldest_ms);
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait as u64).min(cfg.idle_poll)).await;
                    continue;
                }
                p
            }
            Ok(Ok(None)) => {
                tokio::time::sleep(cfg.idle_poll).await;
                continue;
            }
            Ok(Err(e)) => {
                log!("upload queue read failed: {e}");
                tokio::time::sleep(cfg.retry_interval).await;
                continue;
            }
            Err(e) => {
                log!("upload task failed: {e}");
                tokio::time::sleep(cfg.retry_interval).await;
                continue;
            }
        };

        let now = crate::now_ms();
        status.lock().expect("status").last_upload_attempt_ms = Some(now);
        let result = if cfg.is_configured() {
            let manifest = manifest.lock().expect("manifest").clone();
            match build_chunk(&cfg, &pending, &manifest) {
                Ok(chunk) => post(&client, &cfg, &chunk).await,
                Err(e) => Err(format!("building chunk: {e}")),
            }
        } else {
            Err("not configured: set [local] server_url and token".to_string())
        };

        match result {
            Ok(ack) => {
                let through = ack.accepted_through.min(pending.seq_to);
                let db = db.clone();
                let p = pending.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let mut db = db.lock().expect("db");
                    db.mark_uploaded(&p.sess, through)?;
                    if through >= p.seq_to {
                        db.mark_notes_uploaded(&p.note_ids)?;
                    }
                    db.record_upload(&p.sess, p.seq_from, through, true, "ok", crate::now_ms())
                })
                .await;
                let mut st = status.lock().expect("status");
                st.last_upload_ok_ms = Some(crate::now_ms());
                st.last_upload_error = None;
            }
            Err(err) => {
                log!("upload of {} {}..{} failed: {err}; retrying in {:?}", pending.sess, pending.seq_from, pending.seq_to, cfg.retry_interval);
                {
                    let db = db.clone();
                    let p = pending.clone();
                    let detail = err.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        db.lock().expect("db").record_upload(&p.sess, p.seq_from, p.seq_to, false, &detail, crate::now_ms())
                    })
                    .await;
                }
                status.lock().expect("status").last_upload_error = Some(err);
                tokio::time::sleep(cfg.retry_interval).await;
            }
        }
    }
}
