//! Client → server upload contract (`POST /api/v1/chunks`, gzip-compressed JSON,
//! `Authorization: Bearer <player token>`). The server derives every job from
//! these raw frames; nothing the client computed is an input.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

pub const UPLOAD_PROTOCOL: u32 = 1;
pub const CHUNKS_PATH: &str = "/api/v1/chunks";

/// One installed file and its hash (rule 0 install manifest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// `client_binary`, `config_file`, or `plugin` (the plugin DLL/.so in a
    /// game's plugins folder).
    pub role: String,
    pub path: String,
    pub sha256: String,
}

/// A contiguous-by-seq run of one plugin session's raw frames.
#[derive(Debug, Serialize, Deserialize)]
pub struct UploadChunk {
    pub protocol: u32,
    pub client_version: String,
    pub sess: String,
    /// Latest `hello` received for this session, if any.
    pub hello: Option<Box<RawValue>>,
    pub seq_from: u64,
    pub seq_to: u64,
    /// Raw plugin messages in seq order (seq gaps are plugin-side drops).
    /// `modules` messages are redacted (`wire::redact_modules`).
    pub frames: Vec<Box<RawValue>>,
    /// `Config::integrity_hash()` of the client's effective config.
    pub config_hash: String,
    /// `Config::integrity_canonical_json()`, so the server can diff it.
    pub config: Box<RawValue>,
    pub manifest: Vec<ManifestEntry>,
}

/// Server response: every frame of `sess` up to `accepted_through` is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadAck {
    pub sess: String,
    pub accepted_through: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_roundtrip_keeps_frames_verbatim() {
        let frame = r#"{"v":1,"kind":"sample","sess":"s","seq":5,"ch":{"truck.speed":20.5}}"#;
        let chunk = UploadChunk {
            protocol: UPLOAD_PROTOCOL,
            client_version: "0.1.0".into(),
            sess: "s".into(),
            hello: None,
            seq_from: 5,
            seq_to: 5,
            frames: vec![RawValue::from_string(frame.into()).unwrap()],
            config_hash: "ab".into(),
            config: RawValue::from_string("{\"k\":1}".into()).unwrap(),
            manifest: vec![ManifestEntry { role: "plugin".into(), path: "p".into(), sha256: "00".into() }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let back: UploadChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(back.frames[0].get(), frame);
        assert_eq!(back.config.get(), "{\"k\":1}");
        assert_eq!(back.manifest, chunk.manifest);
    }
}
