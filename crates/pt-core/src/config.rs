//! Typed configuration. Mirrors `config/plasmatrack.sample.toml` exactly: unknown
//! keys are rejected so the sample file and the code cannot drift apart.

use serde::{Deserialize, Serialize};

use crate::game::Game;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub config_version: u32,
    pub rules: Rules,
    pub review_policy: ReviewPolicy,
    pub review_flags: ReviewFlags,
    pub timeline: Timeline,
    pub notifications: Notifications,
    pub multiplayer: Multiplayer,
    pub plugin: Plugin,
    pub sync: Sync,
    /// Per-driver settings; the only part excluded from the integrity hash.
    pub local: Local,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub manual_review_over_world_km: f64,
    pub speed_penalty_factor: f64,
    pub points_per_world_mi: f64,
    pub speed: SpeedRules,
    pub distance: DistanceRules,
    pub fuel: FuelRules,
    pub refuel: RefuelHeuristic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedRules {
    pub sustained_window_s: f64,
    pub compare_precision_mph: f64,
    pub ats: SpeedLimits,
    pub ets2: SpeedLimits,
}

impl SpeedRules {
    pub fn limits(&self, game: Game) -> &SpeedLimits {
        match game {
            Game::Ats => &self.ats,
            Game::Ets2 => &self.ets2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedLimits {
    pub penalty_over_mph: f64,
    pub invalid_over_mph: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistanceRules {
    pub compare_precision_km: f64,
    pub odometer_epsilon_km: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuelRules {
    pub negative_fuel_tolerance_l: f64,
    pub mid_job_jump_tolerance_l: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefuelHeuristic {
    pub stationary_below_mph: f64,
    pub min_duration_s: f64,
    pub min_rate_l_per_s: f64,
    pub max_rate_l_per_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewAction {
    /// Manual review: the job is pending and its points are held until an
    /// admin approves or rejects it. Every "flag" in PlasmaTrack means this.
    Review,
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPolicy {
    pub gap: ReviewAction,
    pub late_start: ReviewAction,
    pub trace_checks: ReviewAction,
    pub min_gap_distance_mi: f64,
    pub min_gap_duration_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFlags {
    pub teleport_without_ferry_train_load: bool,
    pub odometer_jump: bool,
    pub impossible_delivery_time: bool,
    pub params: ReviewFlagParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFlagParams {
    pub teleport_min_distance_world_m: f64,
    pub odometer_jump_min_km: f64,
    pub min_plausible_avg_speed_mph: f64,
    pub max_plausible_avg_speed_mph: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub game_time_decrease_min_game: u32,
    pub bracket_min_live_frames_after_started_frames: u32,
    pub bracket_wall_cap_wall_s: f64,
    pub gameplay_event_explain_window_wall_s: f64,
    pub ferry_train_explain_window_wall_s: f64,
    pub match_position_tolerance_world_m: f64,
    pub match_fuel_tolerance_l: f64,
    pub match_game_time_tolerance_min_game: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub sample_rate_hz: f64,
    pub replay_buffer_wall_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    pub retry_interval_wall_s: f64,
}

/// Desktop notifications for a delivery marked Invalid (rule 2) and for the
/// speed penalty (rule 5) activating. The client draws each delay uniformly
/// from [min, max] at microsecond resolution (pt-core itself has no RNG).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notifications {
    pub enabled: bool,
    pub delay_min_wall_s: f64,
    pub delay_max_wall_s: f64,
}

impl Notifications {
    /// Inclusive delay range in whole microseconds.
    pub fn delay_range_us(&self) -> (u64, u64) {
        let us = |s: f64| (s * 1_000_000.0).round() as u64;
        (us(self.delay_min_wall_s), us(self.delay_max_wall_s))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Multiplayer {
    /// Module file names whose presence in the game process means TruckersMP.
    /// PROVISIONAL: filled in from captures (the SDK cannot identify TruckersMP).
    pub truckersmp_module_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Local {
    pub listen_address: String,
    pub port: u16,
    pub server_url: String,
    pub status_port: u16,
    pub token: String,
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Canonical JSON of every setting that must match a server-published
    /// config version: everything except `[local]`. Keys are sorted (serde_json
    /// `Value` maps are ordered), so formatting, comments and key order in the
    /// TOML file do not affect it.
    pub fn integrity_canonical_json(&self) -> String {
        let mut v = serde_json::to_value(self).expect("config always serializes");
        v.as_object_mut()
            .expect("config serializes to an object")
            .remove("local");
        v.to_string()
    }

    /// SHA-256 (lowercase hex) of [`Self::integrity_canonical_json`]. The client
    /// uploads it; the server discards jobs whose hash matches no published
    /// version (rule 0).
    pub fn integrity_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.integrity_canonical_json().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        fn positive(errs: &mut Vec<String>, name: &str, v: f64) {
            if !(v.is_finite() && v > 0.0) {
                errs.push(format!("{name} must be > 0 (got {v})"));
            }
        }
        let mut errs = Vec::new();
        let r = &self.rules;
        for (name, v) in [
            ("rules.manual_review_over_world_km", r.manual_review_over_world_km),
            ("rules.points_per_world_mi", r.points_per_world_mi),
            ("rules.speed.sustained_window_s", r.speed.sustained_window_s),
            ("rules.speed.compare_precision_mph", r.speed.compare_precision_mph),
            ("rules.distance.compare_precision_km", r.distance.compare_precision_km),
            ("rules.distance.odometer_epsilon_km", r.distance.odometer_epsilon_km),
            ("plugin.sample_rate_hz", self.plugin.sample_rate_hz),
            ("sync.retry_interval_wall_s", self.sync.retry_interval_wall_s),
        ] {
            positive(&mut errs, name, v);
        }
        for (g, l) in [("ats", &r.speed.ats), ("ets2", &r.speed.ets2)] {
            positive(&mut errs, &format!("rules.speed.{g}.penalty_over_mph"), l.penalty_over_mph);
            if l.invalid_over_mph <= l.penalty_over_mph {
                errs.push(format!(
                    "rules.speed.{g}: invalid_over_mph ({}) must be above penalty_over_mph ({})",
                    l.invalid_over_mph, l.penalty_over_mph
                ));
            }
        }
        if !(r.speed_penalty_factor > 0.0 && r.speed_penalty_factor <= 1.0) {
            errs.push(format!(
                "rules.speed_penalty_factor must be in (0, 1] (got {})",
                r.speed_penalty_factor
            ));
        }
        if r.fuel.negative_fuel_tolerance_l < 0.0 || r.fuel.mid_job_jump_tolerance_l < 0.0 {
            errs.push("rules.fuel tolerances must be >= 0".into());
        }
        let n = &self.notifications;
        if !(n.delay_min_wall_s >= 0.0 && n.delay_min_wall_s <= n.delay_max_wall_s && n.delay_max_wall_s.is_finite()) {
            errs.push(format!(
                "notifications: need 0 <= delay_min_wall_s ({}) <= delay_max_wall_s ({})",
                n.delay_min_wall_s, n.delay_max_wall_s
            ));
        }
        if r.refuel.min_rate_l_per_s > r.refuel.max_rate_l_per_s {
            errs.push("rules.refuel.min_rate_l_per_s must not exceed max_rate_l_per_s".into());
        }
        if self.local.listen_address != "127.0.0.1" && self.local.listen_address != "::1" {
            errs.push("local.listen_address must be a loopback address".into());
        }
        if self.local.status_port == self.local.port {
            errs.push("local.status_port must differ from local.port".into());
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errs.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../../config/plasmatrack.sample.toml");

    #[test]
    fn sample_config_parses_and_matches_spec_defaults() {
        let c = Config::from_toml_str(SAMPLE).expect("sample config must be valid");
        assert_eq!(c.rules.speed.limits(Game::Ats).penalty_over_mph, 105.0);
        assert_eq!(c.rules.speed.limits(Game::Ats).invalid_over_mph, 125.0);
        assert_eq!(c.rules.speed.limits(Game::Ets2).penalty_over_mph, 100.0);
        assert_eq!(c.rules.speed.limits(Game::Ets2).invalid_over_mph, 150.0);
        assert_eq!(c.rules.manual_review_over_world_km, 150.0);
        assert_eq!(c.rules.speed_penalty_factor, 0.85);
        assert_eq!(c.rules.speed.sustained_window_s, 3.0);
        assert_eq!(c.sync.retry_interval_wall_s, 20.0);
        assert!(!c.review_flags.teleport_without_ferry_train_load);
        assert!(!c.review_flags.odometer_jump);
        assert!(!c.review_flags.impossible_delivery_time);
        assert_eq!(c.review_policy.gap, ReviewAction::Review);
        assert_eq!(c.review_policy.late_start, ReviewAction::Review);
        assert_eq!(c.review_policy.trace_checks, ReviewAction::Review);
        assert!(c.notifications.enabled);
        assert_eq!(c.notifications.delay_range_us(), (5_000_000, 30_000_000));
    }

    #[test]
    fn old_flag_action_is_rejected() {
        let bad = SAMPLE.replace("gap = \"review\"", "gap = \"flag\"");
        assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Parse(_))));
        let bad = SAMPLE.replace("delay_min_wall_s = 5.0", "delay_min_wall_s = 31.0");
        assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn integrity_hash_ignores_local_and_formatting() {
        let base = Config::from_toml_str(SAMPLE).unwrap();
        let h = base.integrity_hash();
        assert_eq!(h.len(), 64);

        let local_changed = SAMPLE
            .replace("port = 30455", "port = 40000")
            .replace("https://plasmatrack.example.invalid", "https://other.invalid");
        assert_eq!(Config::from_toml_str(&local_changed).unwrap().integrity_hash(), h);

        let reformatted = SAMPLE.replace("sustained_window_s = 3.0", "sustained_window_s   =   3.00 # same");
        assert_eq!(Config::from_toml_str(&reformatted).unwrap().integrity_hash(), h);
        let canon = base.integrity_canonical_json();
        for local_key in ["\"local\"", "\"listen_address\"", "\"server_url\"", "\"port\"", "\"token\"", "\"status_port\""] {
            assert!(!canon.contains(local_key), "{local_key} leaked into the integrity hash");
        }
    }

    #[test]
    fn integrity_hash_detects_any_compared_change() {
        let h = Config::from_toml_str(SAMPLE).unwrap().integrity_hash();
        for (from, to) in [
            ("sustained_window_s = 3.0", "sustained_window_s = 3.5"),
            ("invalid_over_mph = 150.0", "invalid_over_mph = 250.0"),
            ("negative_fuel_tolerance_l = 0.5", "negative_fuel_tolerance_l = 50.0"),
            ("gap = \"review\"", "gap = \"off\""),
            ("delay_max_wall_s = 30.0", "delay_max_wall_s = 60.0"),
            ("odometer_jump = false", "odometer_jump = true"),
            ("sample_rate_hz = 20.0", "sample_rate_hz = 5.0"),
            ("retry_interval_wall_s = 20.0", "retry_interval_wall_s = 60.0"),
        ] {
            let changed = SAMPLE.replace(from, to);
            assert_ne!(changed, SAMPLE, "fixture did not apply: {from}");
            let c = Config::from_toml_str(&changed).unwrap();
            assert_ne!(c.integrity_hash(), h, "change not detected: {to}");
        }
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let bad = SAMPLE.replace("[sync]", "[sync]\nretry_interval_s = 5");
        assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn invalid_values_are_rejected() {
        let bad = SAMPLE.replace("invalid_over_mph = 125.0", "invalid_over_mph = 100.0");
        assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Invalid(_))));
        let bad = SAMPLE.replace("listen_address = \"127.0.0.1\"", "listen_address = \"0.0.0.0\"");
        assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Invalid(_))));
    }
}
