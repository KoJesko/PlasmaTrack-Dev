//! The config the server scores with: the current published version
//! (ADMIN_API §11), or the shipped sample until one is published. With no
//! published version every job is discarded by rule 0 anyway.

use pt_core::Config;
use serde_json::json;

use crate::env::{Host, RowExt};
use crate::http::ApiResult;
use crate::store;
use crate::util;

pub const SAMPLE_TOML: &str = include_str!("../../../config/plasmatrack.sample.toml");

pub struct Scoring {
    pub cfg: Config,
    /// `cfg_…` of the current version; `None` while none is published.
    pub version_id: Option<String>,
}

pub fn sample() -> Config {
    Config::from_toml_str(SAMPLE_TOML).expect("the sample config is valid")
}

pub async fn scoring<H: Host>(h: &H) -> ApiResult<Scoring> {
    let row = store::one(h, "SELECT version_id, toml FROM config_versions WHERE current = 1", vec![]).await?;
    Ok(match row.and_then(|r| Config::from_toml_str(&r.s("toml")).ok().map(|c| (r.s("version_id"), c))) {
        Some((id, cfg)) => Scoring { cfg, version_id: Some(id) },
        None => Scoring { cfg: sample(), version_id: None },
    })
}

/// Hash of everything the timeline itself reads: when it changes, stored
/// timelines are stale and the scorer replays from the raw frames.
pub fn timeline_key(cfg: &Config) -> String {
    let parts = json!({
        "timeline": cfg.timeline,
        "speed": cfg.rules.speed,
        "distance": cfg.rules.distance,
        "multiplayer": cfg.multiplayer,
        "review_policy": cfg.review_policy,
    });
    util::sha256_hex(parts.to_string().as_bytes())
}
