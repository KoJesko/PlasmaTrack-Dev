//! Typed configuration. Mirrors `config/plasmatrack.sample.toml` exactly: unknown
//! keys are rejected so the sample file and the code cannot drift apart.

use serde::{Deserialize, Serialize};

use crate::game::Game;
use crate::job::JobType;

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
    pub map: Map,
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
    /// A whole job averaging more than this many 1:1 km per real (unpaused)
    /// minute is held for review (5 = 300 km/h; you, 2026-10-14).
    pub review_over_average_world_km_per_min: f64,
    /// Jobs with less driving time than this are not averaged (too noisy).
    pub average_pace_min_driving_s: f64,
    /// The same check timed by the game's own clock: 1:1 km per minute, where
    /// in-game minutes are turned back into real minutes at the game's time rate
    /// (`local.scale` in singleplayer, `truckersmp_game_min_per_real_min` on
    /// TruckersMP). 5 = 300 km/h. Used in tandem with the above.
    pub review_over_average_world_km_per_game_clock_min: f64,
    /// TruckersMP's server clock: one in-game minute every 10 real seconds.
    pub truckersmp_game_min_per_real_min: f64,
    /// Jobs with fewer in-game minutes of driving than this are not averaged.
    pub average_pace_min_game_min: f64,
    /// A delivery paying more than this per mile of the game's delivery
    /// distance (in the game's currency: USD in ATS, EUR in ETS2) is held for
    /// review (you, 2026-10-16: 400). Absent: not checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_over_income_per_game_mi: Option<f64>,
    /// A delivery paying more than this in all is held for review (you,
    /// 2026-10-16: 500,000). A very long job or a future game version can
    /// pass it honestly, so it is a review, never invalid. Absent: not checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_over_income: Option<f64>,
    pub speed_penalty_factor: f64,
    pub points_per_world_mi: f64,
    pub points_decimals: u32,
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

    /// (penalty, invalid) mph for a job: a road trip's own limits when the
    /// game has them, else the game's.
    pub fn limits_for(&self, game: Game, job_type: JobType) -> (f64, f64) {
        let l = self.limits(game);
        match (&l.road_trip, job_type) {
            (Some(r), JobType::Car) => (r.penalty_over_mph, r.invalid_over_mph),
            _ => (l.penalty_over_mph, l.invalid_over_mph),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedLimits {
    pub penalty_over_mph: f64,
    pub invalid_over_mph: f64,
    /// Road trips (`car_job`, a car or pickup) drive faster than trucks.
    /// Absent: they use the limits above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub road_trip: Option<RoadTripSpeed>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadTripSpeed {
    pub penalty_over_mph: f64,
    pub invalid_over_mph: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistanceRules {
    pub compare_precision_km: f64,
    pub odometer_epsilon_km: f64,
    pub delivered_vs_trace_tolerance_pct: f64,
    pub delivered_vs_trace_tolerance_min_mi: f64,
    pub delivered_vs_trace_short_job_under_mi: f64,
    pub delivered_vs_trace_short_job_max_pct: f64,
    /// `distance.km` is reported in whole km, so the comparison always allows this
    /// much on top of the tolerance (measured: 1.8 tracked vs 1 reported, 238.87 vs
    /// 239, 1232.14 vs 1233).
    pub delivered_distance_resolution_km: f64,
}

impl Rules {
    /// Floors a job's point total to `points_decimals` places (never rounds up).
    /// Rounds away float noise first, so 3.7 stays 3.7 rather than 3.6999… -> 3.6.
    pub fn floor_points(&self, total: f64) -> f64 {
        let scale = 10f64.powi(self.points_decimals as i32);
        ((total * scale * 1e6).round() / 1e6).floor() / scale
    }
}

impl DistanceRules {
    /// Allowed difference, in miles, between the game's `job.delivered` distance and
    /// the tracker's own figure. Everything is measured on the SDK-reported delivery
    /// distance (`delivered_mi`, i.e. `distance.km` in miles): the greater of the
    /// percentage and the floor, except that a job shorter than `short_job_under_mi`
    /// is hard-capped at `short_job_max_pct`.
    pub fn delivered_vs_trace_tolerance_mi(&self, delivered_mi: f64) -> f64 {
        let t = (delivered_mi * self.delivered_vs_trace_tolerance_pct / 100.0)
            .max(self.delivered_vs_trace_tolerance_min_mi);
        if delivered_mi < self.delivered_vs_trace_short_job_under_mi {
            t.min(delivered_mi * self.delivered_vs_trace_short_job_max_pct / 100.0)
        } else {
            t
        }
    }
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
    pub odometer_jump: bool,
    pub impossible_delivery_time: bool,
    pub params: ReviewFlagParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFlagParams {
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
    pub teleport_min_distance_world_m: f64,
    pub match_position_tolerance_world_m: f64,
    pub match_fuel_tolerance_l: f64,
    pub match_game_time_tolerance_min_game: u32,
    /// Finished (or abandoned) jobs older than this, measured on frame wall-clock
    /// time when a new session starts, leave the in-memory timeline (PLAN §7a).
    #[serde(default = "default_reopen_window_wall_h")]
    pub reopen_window_wall_h: f64,
}

fn default_reopen_window_wall_h() -> f64 {
    48.0
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

/// Route map settings (PLAN §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub scale: MapScales,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapScales {
    pub ats: VanillaScale,
    pub ets2: VanillaScale,
}

impl MapScales {
    pub fn vanilla(&self, game: Game) -> &VanillaScale {
        match game {
            Game::Ats => &self.ats,
            Game::Ets2 => &self.ets2,
        }
    }
}

/// The `local.scale` values the unmodded game uses (common_channels.h:13-23).
/// A game can have more than one road scale: ETS2 uses 19, and 15 in the UK
/// (measured 2026-10-13). The first is the main one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VanillaScale {
    pub city_local_scale: f64,
    pub road_local_scales: Vec<f64>,
}

/// How a stretch of route is coloured on the map (PLAN §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleClass {
    City,
    Road,
    OneToOne,
    Nonstandard,
    Unknown,
}

impl VanillaScale {
    /// The main road scale, for jobs too short to measure their own mix.
    pub fn main_road_local_scale(&self) -> f64 {
        self.road_local_scales.first().copied().unwrap_or(1.0)
    }

    pub fn classify(&self, local_scale: Option<f64>) -> ScaleClass {
        let Some(s) = local_scale else { return ScaleClass::Unknown };
        if s == self.city_local_scale {
            ScaleClass::City
        } else if self.road_local_scales.contains(&s) {
            ScaleClass::Road
        } else if s == 1.0 {
            ScaleClass::OneToOne
        } else {
            ScaleClass::Nonstandard
        }
    }
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
    /// Other multiplayer mods, each recognised by its own module names (HaulMP,
    /// 2026-10-22). Absent: none, and the published hash stays as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<MultiplayerMod>,
}

impl Multiplayer {
    /// The mod whose `kind` this is.
    pub fn find_mod(&self, kind: &str) -> Option<&MultiplayerMod> {
        self.mods.iter().find(|m| m.kind == kind)
    }

    /// The server clock rate of mod `kind`, if it has one.
    pub fn mod_clock(&self, kind: Option<&str>) -> Option<f64> {
        kind.and_then(|k| self.find_mod(k)).and_then(|m| m.game_min_per_real_min)
    }

    /// The kind of every mod one of whose module names is in `names`
    /// (case-insensitive), in config order.
    pub fn mods_matching(&self, names: &[&str]) -> Vec<String> {
        self.mods
            .iter()
            .filter(|m| names.iter().any(|n| m.module_names.iter().any(|w| w.eq_ignore_ascii_case(n))))
            .map(|m| m.kind.clone())
            .collect()
    }

    /// The names in `names` that identify a mod (case-insensitive), as given.
    pub fn mod_module_matches<'a>(&self, names: &[&'a str]) -> Vec<&'a str> {
        names
            .iter()
            .copied()
            .filter(|n| self.mods.iter().any(|m| m.module_names.iter().any(|w| w.eq_ignore_ascii_case(n))))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiplayerMod {
    /// The job's `multiplayer.kind` (1-32 lowercase letters, digits, `_`).
    pub kind: String,
    /// Shown to players, e.g. "HaulMP".
    pub name: String,
    /// Any of these module file names loaded in the game process means this
    /// mod for the rest of the session.
    pub module_names: Vec<String>,
    /// The server's game clock, when the mod sets one (TrucklineMP,
    /// 2026-10-23): it replaces `local.scale` in the game-clock pace check,
    /// and save loads are matched without the game time, as on TruckersMP.
    /// Absent: the clock runs as in singleplayer (HaulMP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_min_per_real_min: Option<f64>,
    /// Jobs on this mod are discarded for Singleplayer Division players, the
    /// way TruckersMP jobs always are (you, 2026-10-22).
    pub singleplayer_division_discard: bool,
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
            ("rules.review_over_average_world_km_per_min", r.review_over_average_world_km_per_min),
            ("rules.review_over_income_per_game_mi", r.review_over_income_per_game_mi.unwrap_or(1.0)),
            ("rules.review_over_income", r.review_over_income.unwrap_or(1.0)),
            ("rules.average_pace_min_driving_s", r.average_pace_min_driving_s),
            ("rules.review_over_average_world_km_per_game_clock_min", r.review_over_average_world_km_per_game_clock_min),
            ("rules.truckersmp_game_min_per_real_min", r.truckersmp_game_min_per_real_min),
            ("rules.average_pace_min_game_min", r.average_pace_min_game_min),
            ("rules.points_per_world_mi", r.points_per_world_mi),
            ("rules.speed.sustained_window_s", r.speed.sustained_window_s),
            ("rules.speed.compare_precision_mph", r.speed.compare_precision_mph),
            ("rules.distance.compare_precision_km", r.distance.compare_precision_km),
            ("rules.distance.odometer_epsilon_km", r.distance.odometer_epsilon_km),
            ("rules.distance.delivered_vs_trace_tolerance_pct", r.distance.delivered_vs_trace_tolerance_pct),
            ("rules.distance.delivered_vs_trace_tolerance_min_mi", r.distance.delivered_vs_trace_tolerance_min_mi),
            ("rules.distance.delivered_vs_trace_short_job_under_mi", r.distance.delivered_vs_trace_short_job_under_mi),
            ("rules.distance.delivered_vs_trace_short_job_max_pct", r.distance.delivered_vs_trace_short_job_max_pct),
            ("rules.distance.delivered_distance_resolution_km", r.distance.delivered_distance_resolution_km),
            ("plugin.sample_rate_hz", self.plugin.sample_rate_hz),
            ("sync.retry_interval_wall_s", self.sync.retry_interval_wall_s),
            ("timeline.reopen_window_wall_h", self.timeline.reopen_window_wall_h),
        ] {
            positive(&mut errs, name, v);
        }
        for (g, l) in [("ats", &r.speed.ats), ("ets2", &r.speed.ets2)] {
            let road_trip = l.road_trip.as_ref().map(|t| (format!("{g}.road_trip"), t.penalty_over_mph, t.invalid_over_mph));
            for (name, penalty, invalid) in std::iter::once((g.to_string(), l.penalty_over_mph, l.invalid_over_mph)).chain(road_trip) {
                positive(&mut errs, &format!("rules.speed.{name}.penalty_over_mph"), penalty);
                if invalid <= penalty {
                    errs.push(format!(
                        "rules.speed.{name}: invalid_over_mph ({invalid}) must be above penalty_over_mph ({penalty})"
                    ));
                }
            }
        }
        if !(r.speed_penalty_factor > 0.0 && r.speed_penalty_factor <= 1.0) {
            errs.push(format!(
                "rules.speed_penalty_factor must be in (0, 1] (got {})",
                r.speed_penalty_factor
            ));
        }
        if r.points_decimals > 6 {
            errs.push(format!("rules.points_decimals must be 0..=6 (got {})", r.points_decimals));
        }
        if r.distance.delivered_vs_trace_short_job_max_pct < r.distance.delivered_vs_trace_tolerance_pct {
            errs.push("rules.distance.delivered_vs_trace_short_job_max_pct must be >= delivered_vs_trace_tolerance_pct".into());
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
        let mp = &self.multiplayer;
        for (i, m) in mp.mods.iter().enumerate() {
            let ok_kind = (1..=32).contains(&m.kind.len())
                && m.kind.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && !matches!(m.kind.as_str(), "singleplayer" | "convoy" | "truckersmp" | "unknown");
            if !ok_kind {
                errs.push(format!(
                    "multiplayer.mods[{i}].kind must be 1-32 lowercase letters, digits or _, and not a built-in kind (got {:?})",
                    m.kind
                ));
            }
            if mp.mods[..i].iter().any(|o| o.kind == m.kind) {
                errs.push(format!("multiplayer.mods: kind {:?} appears twice", m.kind));
            }
            if m.name.trim().is_empty() {
                errs.push(format!("multiplayer.mods[{i}].name must not be empty"));
            }
            if m.module_names.is_empty() {
                errs.push(format!("multiplayer.mods[{i}].module_names must not be empty"));
            }
            if let Some(rate) = m.game_min_per_real_min {
                positive(&mut errs, &format!("multiplayer.mods[{i}].game_min_per_real_min"), rate);
            }
            for n in &m.module_names {
                let elsewhere = mp.truckersmp_module_names.iter().any(|t| t.eq_ignore_ascii_case(n))
                    || mp.mods[..i].iter().any(|o| o.module_names.iter().any(|t| t.eq_ignore_ascii_case(n)));
                if elsewhere {
                    errs.push(format!("multiplayer: module name {n:?} belongs to more than one kind"));
                }
            }
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
        assert_eq!(c.rules.speed.limits_for(Game::Ats, JobType::Car), (155.0, 200.0));
        assert_eq!(c.rules.speed.limits_for(Game::Ats, JobType::Freight), (105.0, 125.0));
        assert_eq!(c.rules.speed.limits_for(Game::Ets2, JobType::Car), (100.0, 150.0));
        assert_eq!(c.rules.manual_review_over_world_km, 150.0);
        assert_eq!(c.rules.speed_penalty_factor, 0.85);
        assert_eq!(c.rules.speed.sustained_window_s, 3.0);
        assert_eq!(c.sync.retry_interval_wall_s, 20.0);
        assert!(!c.review_flags.odometer_jump);
        assert!(!c.review_flags.impossible_delivery_time);
        assert_eq!(c.review_policy.gap, ReviewAction::Review);
        assert_eq!(c.review_policy.late_start, ReviewAction::Review);
        assert_eq!(c.review_policy.trace_checks, ReviewAction::Review);
        assert!(c.notifications.enabled);
        assert_eq!(c.notifications.delay_range_us(), (5_000_000, 30_000_000));
    }

    #[test]
    fn scale_classes() {
        let c = Config::from_toml_str(SAMPLE).unwrap();
        let ats = c.map.scale.vanilla(Game::Ats);
        let ets2 = c.map.scale.vanilla(Game::Ets2);
        assert_eq!(ats.classify(Some(20.0)), ScaleClass::Road);
        assert_eq!(ets2.classify(Some(19.0)), ScaleClass::Road);
        assert_eq!(ets2.classify(Some(15.0)), ScaleClass::Road, "ETS2 UK roads");
        assert_eq!(ats.classify(Some(15.0)), ScaleClass::Nonstandard);
        assert_eq!(ets2.main_road_local_scale(), 19.0);
        assert_eq!(ets2.classify(Some(20.0)), ScaleClass::Nonstandard);
        assert_eq!(ats.classify(Some(3.0)), ScaleClass::City);
        assert_eq!(ats.classify(Some(1.0)), ScaleClass::OneToOne);
        assert_eq!(ats.classify(Some(10.0)), ScaleClass::Nonstandard);
        assert_eq!(ats.classify(None), ScaleClass::Unknown);
    }

    #[test]
    fn points_floor_to_one_decimal() {
        let r = Config::from_toml_str(SAMPLE).unwrap().rules;
        assert_eq!(r.points_decimals, 1);
        assert_eq!(r.floor_points(3.70), 3.7);
        assert_eq!(r.floor_points(3.79), 3.7);
        assert_eq!(r.floor_points(0.1 + 0.2), 0.3); // 0.30000000000000004
        assert_eq!(r.floor_points(3.0 * 1.1), 3.3); // 3.3000000000000003
        assert_eq!(r.floor_points(0.0), 0.0);
    }

    #[test]
    fn delivered_vs_trace_tolerance() {
        let d = Config::from_toml_str(SAMPLE).unwrap().rules.distance;
        let t = |mi: f64| d.delivered_vs_trace_tolerance_mi(mi);
        // Delivered distance under 100 mi: the 30 mi floor is hard-capped at 5%.
        assert!((t(10.0) - 0.5).abs() < 1e-9);
        assert!((t(99.0) - 4.95).abs() < 1e-9);
        // From 100 mi: greater of 1% and 30 mi.
        assert_eq!(t(100.0), 30.0);
        assert_eq!(t(2999.0), 30.0);
        assert!((t(5000.0) - 50.0).abs() < 1e-9);
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
            ("singleplayer_division_discard = true", "singleplayer_division_discard = false"),
            ("\"haulmp-telemetry.dll\"]", "\"haulmp-telemetry.dll\", \"haulmp_extra.dll\"]"),
        ] {
            let changed = SAMPLE.replace(from, to);
            assert_ne!(changed, SAMPLE, "fixture did not apply: {from}");
            let c = Config::from_toml_str(&changed).unwrap();
            assert_ne!(c.integrity_hash(), h, "change not detected: {to}");
        }
    }

    const HAULMP: &str = "[[multiplayer.mods]]\nkind = \"haulmp\"\nname = \"HaulMP\"\nmodule_names = [\"haulmp_stub.dll\", \"haulmp-telemetry.dll\"]\nsingleplayer_division_discard = true\n";

    #[test]
    fn multiplayer_mods_are_optional_and_checked() {
        let c = Config::from_toml_str(SAMPLE).unwrap();
        let m = c.multiplayer.find_mod("haulmp").expect("sample lists HaulMP");
        assert!(m.singleplayer_division_discard);
        assert_eq!(c.multiplayer.mods_matching(&["KERNEL32.DLL", "HaulMP_Stub.dll"]), vec!["haulmp".to_string()]);

        // Without mods the canonical JSON has no `mods` key, so configs
        // published before mods existed keep their hash.
        assert!(SAMPLE.contains(HAULMP), "sample's HaulMP block changed; update HAULMP");
        let from = SAMPLE.find("# HaulMP (ETS2)").expect("first mod block");
        let to = from + SAMPLE[from..].find("# ----").expect("next section");
        let without = Config::from_toml_str(&format!("{}{}", &SAMPLE[..from], &SAMPLE[to..])).unwrap();
        assert!(without.multiplayer.mods.is_empty());
        assert!(!without.integrity_canonical_json().contains("\"mods\""));

        for (from, to) in [
            ("kind = \"haulmp\"", "kind = \"convoy\""),
            ("kind = \"haulmp\"", "kind = \"HaulMP\""),
            ("kind = \"haulmp\"", "kind = \"\""),
            ("name = \"HaulMP\"", "name = \" \""),
            ("[\"haulmp_stub.dll\", \"haulmp-telemetry.dll\"]", "[]"),
            ("[\"haulmp_stub.dll\", \"haulmp-telemetry.dll\"]", "[\"Core_ETS2MP.dll\"]"),
        ] {
            let bad = SAMPLE.replace(HAULMP, &HAULMP.replace(from, to));
            assert!(matches!(Config::from_toml_str(&bad), Err(ConfigError::Invalid(_))), "accepted: {to}");
        }
        let twice = SAMPLE.replace(HAULMP, &format!("{HAULMP}{}", HAULMP.replace("haulmp_stub", "other")));
        assert!(matches!(Config::from_toml_str(&twice), Err(ConfigError::Invalid(_))), "duplicate kind accepted");
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
