//! Plugin wire protocol (docs/WIRE.md): length-prefixed JSON frames.
//!
//! Shared by the client (which receives frames from the plugin) and the server
//! (which receives the same raw frames inside upload chunks).

use serde::{Deserialize, Serialize};

use crate::config::Multiplayer;

/// Largest frame the client accepts from the plugin. A capture-mode snapshot is
/// well below this.
pub const MAX_FRAME_BYTES: usize = 64 << 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    Oversized(usize),
    #[error("frame is not UTF-8")]
    NotUtf8,
    #[error("frame is not a JSON object with an envelope: {0}")]
    BadEnvelope(String),
}

/// Encodes one frame: u32 little-endian length, then the JSON bytes.
pub fn encode_frame(json: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    out
}

/// Incremental decoder for a byte stream of frames.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame, `Ok(None)` if more bytes are needed.
    /// After an error the stream is unusable and the connection must be dropped.
    pub fn next_frame(&mut self) -> Result<Option<String>, WireError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(WireError::Oversized(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let payload: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
        String::from_utf8(payload).map(Some).map_err(|_| WireError::NotUtf8)
    }

    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// Envelope fields common to all plugin messages. `seq` is absent on `hello`
/// and `snapshot`; `frame`/`sim_us` are absent on sender-thread messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub kind: String,
    pub sess: String,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub wall_ms: Option<i64>,
    #[serde(default)]
    pub frame: Option<u64>,
    #[serde(default)]
    pub sim_us: Option<u64>,
}

impl Envelope {
    pub fn parse(json: &str) -> Result<Envelope, WireError> {
        let env: Envelope = serde_json::from_str(json).map_err(|e| WireError::BadEnvelope(e.to_string()))?;
        if env.sess.len() != 32 || !env.sess.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(WireError::BadEnvelope(format!("bad session id {:?}", env.sess)));
        }
        Ok(env)
    }
}

/// The acknowledgement the client sends back to the plugin: everything of
/// `sess` up to and including `seq` is persisted.
pub fn ack_json(sess: &str, seq: u64) -> String {
    let sess = serde_json::to_string(sess).expect("strings always serialize");
    format!("{{\"kind\":\"ack\",\"sess\":{sess},\"seq\":{seq}}}")
}

/// Privacy: a `modules` message lists every DLL loaded in the game process.
/// Before anything leaves the player's PC it is reduced to the TruckersMP
/// matches (case-insensitive), the matches of `[[multiplayer.mods]]` names
/// (`mod_matches`, only when mods are configured) and a count. Other messages
/// are returned as-is.
pub fn redact_modules(json: &str, mp: &Multiplayer) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if v.get("kind").and_then(|k| k.as_str()) != Some("modules") {
        return json.to_string();
    }
    let obj = v.as_object_mut().expect("checked above");
    let modules: Vec<String> = obj
        .remove("modules")
        .and_then(|m| serde_json::from_value(m).ok())
        .unwrap_or_default();
    let wanted: Vec<String> = mp.truckersmp_module_names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let matches: Vec<&String> = modules
        .iter()
        .filter(|m| wanted.contains(&m.to_ascii_lowercase()))
        .collect();
    obj.insert("module_count".into(), modules.len().into());
    obj.insert("truckersmp_matches".into(), serde_json::json!(matches));
    if !mp.mods.is_empty() {
        let names: Vec<&str> = modules.iter().map(String::as_str).collect();
        obj.insert("mod_matches".into(), serde_json::json!(mp.mod_module_matches(&names)));
    }
    obj.insert("redacted".into(), true.into());
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MultiplayerMod;

    const SESS: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn frames_roundtrip_across_arbitrary_splits() {
        let msgs = ["{\"a\":1}", "", "{\"k\":\"K\u{f6}ln\"}"];
        let mut bytes = Vec::new();
        for m in msgs {
            bytes.extend(encode_frame(m));
        }
        for split in 0..bytes.len() {
            let mut d = FrameDecoder::new();
            let mut got = Vec::new();
            for chunk in [&bytes[..split], &bytes[split..]] {
                d.push(chunk);
                while let Some(f) = d.next_frame().unwrap() {
                    got.push(f);
                }
            }
            assert_eq!(got, msgs, "split at {split}");
            assert_eq!(d.buffered(), 0);
        }
    }

    #[test]
    fn oversized_and_non_utf8_frames_are_errors() {
        let mut d = FrameDecoder::new();
        d.push(&((MAX_FRAME_BYTES as u32) + 1).to_le_bytes());
        assert!(matches!(d.next_frame(), Err(WireError::Oversized(_))));
        let mut d = FrameDecoder::new();
        d.push(&2u32.to_le_bytes());
        d.push(&[0xff, 0xfe]);
        assert_eq!(d.next_frame(), Err(WireError::NotUtf8));
    }

    #[test]
    fn envelope_parsing() {
        let e = Envelope::parse(&format!(
            "{{\"v\":1,\"kind\":\"sample\",\"sess\":\"{SESS}\",\"seq\":7,\"wall_ms\":1,\"frame\":3,\"sim_us\":16667,\"ch\":{{}}}}"
        ))
        .unwrap();
        assert_eq!((e.kind.as_str(), e.seq, e.sim_us), ("sample", Some(7), Some(16667)));
        let hello = Envelope::parse(&format!("{{\"v\":1,\"kind\":\"hello\",\"sess\":\"{SESS}\"}}")).unwrap();
        assert_eq!(hello.seq, None);
        assert!(Envelope::parse("{\"v\":1,\"kind\":\"x\",\"sess\":\"short\"}").is_err());
        assert!(Envelope::parse("[1,2]").is_err());
    }

    #[test]
    fn modules_are_redacted_to_truckersmp_matches() {
        let raw = format!(
            "{{\"v\":1,\"kind\":\"modules\",\"sess\":\"{SESS}\",\"seq\":3,\"modules\":[\"KERNEL32.DLL\",\"Core_ATSMP.dll\",\"secret-mod.dll\"]}}"
        );
        let tmp = Multiplayer { truckersmp_module_names: vec!["core_atsmp.dll".into()], mods: vec![] };
        let out = redact_modules(&raw, &tmp);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("modules").is_none());
        assert_eq!(v["module_count"], 3);
        assert_eq!(v["truckersmp_matches"], serde_json::json!(["Core_ATSMP.dll"]));
        assert!(v.get("mod_matches").is_none(), "no mods configured: the message is as before");
        assert_eq!(v["seq"], 3);
        assert!(!out.contains("secret-mod"));
        // Non-module messages pass through unchanged.
        let sample = format!("{{\"kind\":\"sample\",\"sess\":\"{SESS}\",\"seq\":4}}");
        assert_eq!(redact_modules(&sample, &tmp), sample);
    }

    #[test]
    fn modules_keep_multiplayer_mod_matches() {
        let raw = format!(
            "{{\"v\":1,\"kind\":\"modules\",\"sess\":\"{SESS}\",\"seq\":3,\"modules\":[\"KERNEL32.DLL\",\"haulmp_stub.dll\",\"secret-mod.dll\"]}}"
        );
        let mp = Multiplayer {
            truckersmp_module_names: vec!["core_ets2mp.dll".into()],
            mods: vec![MultiplayerMod {
                kind: "haulmp".into(),
                name: "HaulMP".into(),
                module_names: vec!["haulmp_stub.dll".into(), "haulmp-telemetry.dll".into()],
                singleplayer_division_discard: true,
            }],
        };
        let out = redact_modules(&raw, &mp);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["truckersmp_matches"], serde_json::json!([]));
        assert_eq!(v["mod_matches"], serde_json::json!(["haulmp_stub.dll"]));
        assert_eq!(v["module_count"], 3);
        assert!(!out.contains("secret-mod"));
    }

    #[test]
    fn ack_format_matches_plugin_parser() {
        assert_eq!(ack_json(SESS, 42), format!("{{\"kind\":\"ack\",\"sess\":\"{SESS}\",\"seq\":42}}"));
    }
}
