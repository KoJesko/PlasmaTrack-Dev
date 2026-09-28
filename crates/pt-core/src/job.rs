//! Job identity (PLAN §5 "Job identity") and the delivery event.
//!
//! Attribute names: common/scssdk_telemetry_common_configs.h (job config) and
//! common/scssdk_telemetry_common_gameplay_events.h (job.delivered).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::game::Game;
use crate::trace::{attr, Attr};

/// Where the job leads and when it is due. Stays the same for a job's whole
/// life, across save loads (verified byte-identical over six loads, PLAN §3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteKey {
    pub game: Game,
    pub source_city: String,
    pub source_company: String,
    pub destination_city: String,
    pub destination_company: String,
    /// Absolute in-game deadline (`delivery.time`, minutes).
    pub delivery_time: Option<u64>,
    pub planned_distance_km: Option<u64>,
    pub job_market: String,
    pub is_special_job: Option<bool>,
}

/// What is hauled. A different cargo key on the same route is a `cargo_switch`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CargoKey {
    pub cargo_id: String,
    /// `cargo.mass` in kg, compared exactly as the game reports it (f32 bits).
    pub cargo_mass_bits: Option<u64>,
    pub cargo_unit_count: Option<u64>,
    pub income: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Fingerprint {
    pub route: RouteKey,
    pub cargo: CargoKey,
}

/// A parsed, non-empty `job` config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub fingerprint: Fingerprint,
    pub cargo_name: String,
    pub cargo_mass_kg: Option<f64>,
    /// `cargo.loaded`: false while driving to the pickup with an owned trailer.
    pub cargo_loaded: bool,
}

/// `job.market` values (configs.h:627-641).
pub fn is_world_of_trucks_market(market: &str) -> bool {
    matches!(market, "external_contracts" | "external_market")
}

impl JobInfo {
    /// `None` for an empty job config ("no job", configs.h:125).
    pub fn from_config(game: Game, attrs: &[Attr]) -> Option<JobInfo> {
        if attrs.is_empty() {
            return None;
        }
        let s = |k: &str| attr(attrs, k).and_then(Value::as_str).unwrap_or_default().to_string();
        let u = |k: &str| attr(attrs, k).and_then(Value::as_u64);
        let mass = attr(attrs, "cargo.mass").and_then(Value::as_f64);
        Some(JobInfo {
            fingerprint: Fingerprint {
                route: RouteKey {
                    game,
                    source_city: s("source.city.id"),
                    source_company: s("source.company.id"),
                    destination_city: s("destination.city.id"),
                    destination_company: s("destination.company.id"),
                    delivery_time: u("delivery.time"),
                    planned_distance_km: u("planned_distance.km"),
                    job_market: s("job.market"),
                    is_special_job: attr(attrs, "is.special.job").and_then(Value::as_bool),
                },
                cargo: CargoKey {
                    cargo_id: s("cargo.id"),
                    cargo_mass_bits: mass.map(|m| (m as f32).to_bits() as u64),
                    cargo_unit_count: u("cargo.unit.count"),
                    income: u("income"),
                },
            },
            cargo_name: s("cargo"),
            cargo_mass_kg: mass,
            // Older game versions lack the attribute; then the cargo is loaded.
            cargo_loaded: attr(attrs, "cargo.loaded").and_then(Value::as_bool).unwrap_or(true),
        })
    }

    pub fn is_world_of_trucks(&self) -> bool {
        is_world_of_trucks_market(&self.fingerprint.route.job_market)
    }
}

/// The `job.delivered` gameplay event (gameplay_events.h:20-35, 100-132).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delivered {
    /// The game's own job distance: game scale, includes the empty drive to
    /// the pickup, whole km, and taken from the save (so it can be edited).
    pub distance_km: Option<f64>,
    pub revenue: Option<i64>,
    pub earned_xp: Option<i64>,
    pub cargo_damage: Option<f64>,
    /// In-game minutes the job took.
    pub delivery_time_min: Option<u64>,
    pub auto_park_used: Option<bool>,
    pub auto_load_used: Option<bool>,
}

impl Delivered {
    pub fn from_attrs(attrs: &[Attr]) -> Delivered {
        Delivered {
            distance_km: attr(attrs, "distance.km").and_then(Value::as_f64),
            revenue: attr(attrs, "revenue").and_then(Value::as_i64),
            earned_xp: attr(attrs, "earned.xp").and_then(Value::as_i64),
            cargo_damage: attr(attrs, "cargo.damage").and_then(Value::as_f64),
            delivery_time_min: attr(attrs, "delivery.time").and_then(Value::as_u64),
            auto_park_used: attr(attrs, "auto.park.used").and_then(Value::as_bool),
            auto_load_used: attr(attrs, "auto.load.used").and_then(Value::as_bool),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a(name: &str, value: Value) -> Attr {
        Attr { name: name.into(), index: None, value }
    }

    fn tyres(loaded: bool, cargo: &str) -> Vec<Attr> {
        vec![
            a("cargo.id", json!(cargo)),
            a("cargo.mass", json!(18257.25)),
            a("cargo.unit.count", json!(25)),
            a("source.city.id", json!("port_angeles")),
            a("source.company.id", json!("apa_car_whs")),
            a("destination.city.id", json!("tacoma")),
            a("destination.company.id", json!("pt_trk_dlr")),
            a("income", json!(11626)),
            a("delivery.time", json!(10401816)),
            a("planned_distance.km", json!(235)),
            a("cargo.loaded", json!(loaded)),
            a("job.market", json!("cargo_market")),
            a("is.special.job", json!(false)),
        ]
    }

    #[test]
    fn cargo_loaded_is_not_part_of_the_fingerprint() {
        let before = JobInfo::from_config(Game::Ats, &tyres(false, "tyres")).unwrap();
        let after = JobInfo::from_config(Game::Ats, &tyres(true, "tyres")).unwrap();
        assert!(!before.cargo_loaded && after.cargo_loaded);
        assert_eq!(before.fingerprint, after.fingerprint);
        assert!(JobInfo::from_config(Game::Ats, &[]).is_none());
    }

    #[test]
    fn cargo_switch_keeps_the_route_key() {
        let a1 = JobInfo::from_config(Game::Ats, &tyres(true, "tyres")).unwrap();
        let a2 = JobInfo::from_config(Game::Ats, &tyres(true, "dynamite")).unwrap();
        assert_eq!(a1.fingerprint.route, a2.fingerprint.route);
        assert_ne!(a1.fingerprint.cargo, a2.fingerprint.cargo);
    }

    #[test]
    fn delivered_attributes() {
        // From the clean Sioux Falls delivery (PLAN §6).
        let d = Delivered::from_attrs(&[
            a("revenue", json!(68980)),
            a("distance.km", json!(1233)),
            a("delivery.time", json!(350)),
            a("auto.park.used", json!(true)),
        ]);
        assert_eq!(d.distance_km, Some(1233.0));
        assert_eq!(d.delivery_time_min, Some(350));
        assert_eq!(d.auto_load_used, None);
    }
}
