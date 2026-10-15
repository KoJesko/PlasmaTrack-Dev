//! A job's stats sheet (ADMIN_API §14): the fields a player submits for a
//! manual job and a moderator may edit on any job, in canonical units
//! (km, litres, Unix ms). Also the manual distance rule: game distance ÷ map
//! scale, with the UK stretch and an adjusted section at their own scales
//! (project owner, 2026-10-15).

use pt_core::config::ScaleClass;
use pt_core::{Config, Game};
use serde_json::{json, Map, Value};

use crate::http::{ApiError, ApiResult};
use crate::util::{self, round2, round3};

/// ETS2's UK roads (the game's own `local.scale` there).
pub const UK_SCALE: f64 = 15.0;

pub const KM_PER_MI: f64 = 1.609344;
const L_PER_US_GAL: f64 = 3.785411784;
const L_PER_UK_GAL: f64 = 4.54609;

/// Text fields: canonical key ← request key.
const TEXT: [(&str, &str); 8] = [
    ("source_city", "start_city"),
    ("source_company", "start_company"),
    ("destination_city", "end_city"),
    ("destination_company", "end_company"),
    ("cargo_name", "cargo_name"),
    ("truck", "truck"),
    ("truck_model", "truck_model"),
    ("bonus_id", "bonus_id"),
];

/// Every request key a sheet accepts (PATCH, approval corrections).
pub const EDIT_KEYS: [&str; 18] = [
    "start_city", "start_company", "end_city", "end_company", "cargo_name", "truck", "truck_model", "bonus_id",
    "distance", "fuel_used", "damage", "profit", "started_at", "ended_at", "mod_names", "map_scale", "uk", "adjusted_scale",
];

pub const MAX_TEXT: usize = 100;
pub const MAX_MODS: usize = 100;
pub const MAX_DISTANCE_KM: f64 = 50_000.0;

#[derive(Default)]
struct Errs(Map<String, Value>);

impl Errs {
    fn add(&mut self, k: &str, m: &str) {
        self.0.entry(k.to_string()).or_insert_with(|| m.into());
    }
}

fn text(v: &Value, key: &str, errs: &mut Errs) -> Option<String> {
    match v.as_str().map(str::trim) {
        Some(s) if (1..=MAX_TEXT).contains(&s.chars().count()) => Some(s.to_string()),
        _ => {
            errs.add(key, &format!("1-{MAX_TEXT} characters"));
            None
        }
    }
}

fn number(v: &Value, key: &str, lo: f64, hi: f64, errs: &mut Errs) -> Option<f64> {
    match v.as_f64() {
        Some(x) if x.is_finite() && x >= lo && x <= hi => Some(x),
        _ => {
            errs.add(key, &format!("a number from {lo} to {hi}"));
            None
        }
    }
}

fn positive(v: &Value, key: &str, hi: f64, errs: &mut Errs) -> Option<f64> {
    match v.as_f64() {
        Some(x) if x.is_finite() && x > 0.0 && x <= hi => Some(x),
        _ => {
            errs.add(key, &format!("a number above 0, at most {hi}"));
            None
        }
    }
}

fn instant(v: &Value, key: &str, errs: &mut Errs) -> Option<i64> {
    match v.as_str().and_then(util::parse_rfc3339) {
        Some(t) => Some(t),
        None => {
            errs.add(key, "an RFC 3339 instant");
            None
        }
    }
}

/// Request distance → km in the given unit.
fn km(x: f64, unit: Option<&str>) -> f64 {
    if unit == Some("mi") {
        x * KM_PER_MI
    } else {
        x
    }
}

/// Parses the sheet fields of a request body into canonical form. `full`: a
/// submission (every required field); otherwise a partial edit, where `null`
/// clears a field. `base` is the effective sheet the edit applies to (for
/// partial `uk` and `adjusted_scale` objects).
pub fn parse(body: &Value, game: Game, full: bool, base: &Map<String, Value>) -> ApiResult<Map<String, Value>> {
    let mut e = Errs::default();
    let mut out = Map::new();
    let unit = body.get("unit").and_then(Value::as_str);
    let fuel_unit = body.get("fuel_unit").and_then(Value::as_str);
    if let Some(u) = body.get("unit").filter(|v| !v.is_null()) {
        if !matches!(u.as_str(), Some("mi" | "km")) {
            e.add("unit", "mi or km");
        }
    }
    if let Some(u) = body.get("fuel_unit").filter(|v| !v.is_null()) {
        if !matches!(u.as_str(), Some("l" | "us_gal" | "uk_gal")) {
            e.add("fuel_unit", "l, us_gal or uk_gal");
        }
    }
    let has = |k: &str| body.get(k).is_some();
    let section_distance = |k: &str| body.get(k).and_then(|s| s.get("distance")).is_some();
    let needs_unit = has("distance") || section_distance("uk") || section_distance("adjusted_scale");
    if (full || needs_unit) && unit.is_none() {
        e.add("unit", "required with distances");
    }
    if (full || has("fuel_used")) && fuel_unit.is_none() && !body.get("fuel_used").is_some_and(Value::is_null) {
        e.add("fuel_unit", "required with fuel_used");
    }
    let required = |k: &str, e: &mut Errs| {
        if full && !has(k) {
            e.add(k, "required");
        }
    };

    for (canon, key) in TEXT {
        match body.get(key) {
            None => {
                if full && !matches!(key, "bonus_id" | "truck_model") {
                    required(key, &mut e);
                }
            }
            Some(Value::Null) if matches!(key, "bonus_id" | "truck_model") || (!full && key == "truck") => {
                out.insert(canon.into(), Value::Null);
            }
            Some(v) => {
                if let Some(s) = text(v, key, &mut e) {
                    out.insert(canon.into(), s.into());
                }
            }
        }
    }
    if full {
        // A player never picks a bonus: moderators do (owner, 2026-10-15).
        if has("bonus_id") {
            e.add("bonus_id", "set by moderators only");
        }
        out.remove("bonus_id");
    }

    let nullable = |k: &str| !full && body.get(k).is_some_and(Value::is_null);
    if nullable("distance") {
        e.add("distance", "cannot be cleared");
    } else if let Some(v) = body.get("distance") {
        if let Some(x) = positive(v, "distance", MAX_DISTANCE_KM, &mut e) {
            out.insert("distance_km".into(), json!(km(x, unit)));
        }
    } else {
        required("distance", &mut e);
    }
    if nullable("fuel_used") {
        out.insert("fuel_used_l".into(), Value::Null);
    } else if let Some(v) = body.get("fuel_used") {
        if let Some(x) = number(v, "fuel_used", 0.0, 1_000_000.0, &mut e) {
            let l = match fuel_unit {
                Some("us_gal") => x * L_PER_US_GAL,
                Some("uk_gal") => x * L_PER_UK_GAL,
                _ => x,
            };
            out.insert("fuel_used_l".into(), json!(l));
        }
    } else {
        required("fuel_used", &mut e);
    }
    if nullable("damage") {
        out.insert("damage_pct".into(), Value::Null);
    } else if let Some(v) = body.get("damage") {
        if let Some(x) = number(v, "damage", 0.0, 100.0, &mut e) {
            out.insert("damage_pct".into(), json!(x));
        }
    } else {
        required("damage", &mut e);
    }
    if nullable("profit") {
        out.insert("profit".into(), Value::Null);
    } else if let Some(v) = body.get("profit") {
        if let Some(x) = number(v, "profit", -100_000_000.0, 100_000_000.0, &mut e) {
            out.insert("profit".into(), json!(x));
        }
    } else {
        required("profit", &mut e);
    }
    for (canon, key) in [("started_at_ms", "started_at"), ("ended_at_ms", "ended_at")] {
        if nullable(key) {
            e.add(key, "cannot be cleared");
        } else if let Some(v) = body.get(key) {
            if let Some(t) = instant(v, key, &mut e) {
                out.insert(canon.into(), json!(t));
            }
        } else {
            required(key, &mut e);
        }
    }
    if nullable("mod_names") {
        out.insert("mods".into(), json!([]));
    } else if let Some(v) = body.get("mod_names") {
        match v.as_array() {
            Some(a) if a.len() <= MAX_MODS => {
                let mods: Vec<Value> = a.iter().filter_map(|m| text(m, "mod_names", &mut e)).map(Value::from).collect();
                out.insert("mods".into(), Value::Array(mods));
            }
            _ => e.add("mod_names", &format!("a list of at most {MAX_MODS} names")),
        }
    } else if full {
        out.insert("mods".into(), json!([]));
    }
    if nullable("map_scale") {
        out.insert("map_scale".into(), Value::Null);
    } else if let Some(v) = body.get("map_scale") {
        if let Some(x) = positive(v, "map_scale", 1000.0, &mut e) {
            out.insert("map_scale".into(), json!(x));
        }
    }

    // The UK stretch (ETS2) and an adjusted-scale section: whole objects on a
    // submission, field by field on an edit.
    for (key, fields) in [
        ("uk", &["distance", "entry_city", "end_city", "route_evidence"][..]),
        ("adjusted_scale", &["entry_city", "exit_city", "distance", "scale", "evidence"][..]),
    ] {
        let Some(v) = body.get(key) else { continue };
        if v.is_null() {
            out.insert(key.into(), Value::Null);
            continue;
        }
        if key == "uk" && game != Game::Ets2 {
            e.add("uk", "ETS2 only");
            continue;
        }
        let Some(o) = v.as_object() else {
            e.add(key, "an object");
            continue;
        };
        let mut sec = match (full, base.get(key)) {
            (false, Some(Value::Object(b))) => b.clone(),
            _ => Map::new(),
        };
        for (k, x) in o {
            let path = format!("{key}.{k}");
            match k.as_str() {
                "distance" => {
                    if let Some(d) = positive(x, &path, MAX_DISTANCE_KM, &mut e) {
                        sec.insert("distance_km".into(), json!(km(d, unit)));
                    }
                }
                "scale" => {
                    if let Some(s) = positive(x, &path, 1000.0, &mut e) {
                        sec.insert("scale".into(), json!(s));
                    }
                }
                "route_evidence" | "evidence" => {
                    if !full {
                        e.add(&path, "evidence cannot be edited");
                    } else if let Some(s) = x.as_str() {
                        sec.insert("evidence_id".into(), s.into());
                    } else {
                        e.add(&path, "an evidence id");
                    }
                }
                _ if fields.contains(&k.as_str()) => {
                    if let Some(s) = text(x, &path, &mut e) {
                        sec.insert(k.clone(), s.into());
                    }
                }
                _ => e.add(&path, "unknown field"),
            }
        }
        let want: &[&str] = if key == "uk" {
            &["distance_km", "entry_city", "end_city", "evidence_id"]
        } else {
            &["entry_city", "exit_city", "distance_km", "scale", "evidence_id"]
        };
        for w in want {
            if !sec.contains_key(*w) {
                let name = match *w {
                    "distance_km" => "distance",
                    "evidence_id" if key == "uk" => "route_evidence",
                    "evidence_id" => "evidence",
                    x => x,
                };
                e.add(&format!("{key}.{name}"), "required");
            }
        }
        out.insert(key.into(), Value::Object(sec));
    }
    if !e.0.is_empty() {
        return Err(ApiError::validation(Value::Object(e.0)));
    }
    Ok(out)
}

/// Rejects body keys a PATCH does not know (besides its control fields).
pub fn check_keys(body: &Value, control: &[&str]) -> ApiResult<()> {
    let mut e = Errs::default();
    for k in body.as_object().into_iter().flatten().map(|(k, _)| k) {
        if !EDIT_KEYS.contains(&k.as_str()) && !control.contains(&k.as_str()) && !matches!(k.as_str(), "unit" | "fuel_unit") {
            e.add(k, "unknown field");
        }
    }
    if e.0.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(Value::Object(e.0)))
    }
}

/// `base` with `overrides` laid over it, key by key.
pub fn merged(base: &Map<String, Value>, overrides: &Map<String, Value>) -> Map<String, Value> {
    let mut m = base.clone();
    for (k, v) in overrides {
        m.insert(k.clone(), v.clone());
    }
    m
}

/// Cross-field checks on an effective sheet.
pub fn check(sheet: &Map<String, Value>) -> ApiResult<()> {
    let mut e = Errs::default();
    let f = |v: Option<&Value>| v.and_then(Value::as_f64).unwrap_or(0.0);
    let total = f(sheet.get("distance_km"));
    let uk = f(sheet.get("uk").and_then(|u| u.get("distance_km")));
    let adj = f(sheet.get("adjusted_scale").and_then(|u| u.get("distance_km")));
    if uk + adj > total + 1e-9 {
        e.add(if adj > 0.0 { "adjusted_scale.distance" } else { "uk.distance" }, "the UK and adjusted sections are part of distance, so cannot exceed it");
    }
    if let (Some(a), Some(b)) = (sheet.get("started_at_ms").and_then(Value::as_i64), sheet.get("ended_at_ms").and_then(Value::as_i64)) {
        if b <= a {
            e.add("ended_at", "must be after started_at");
        }
    }
    if e.0.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(Value::Object(e.0)))
    }
}

/// World (1:1) distance of a sheet: (distance − UK − adjusted) ÷ map scale,
/// UK ÷ 15, adjusted ÷ its scale. Returns (world km, per scale class: game km,
/// world km).
pub fn world_km(sheet: &Map<String, Value>, game: Game, cfg: &Config) -> (f64, Vec<(ScaleClass, f64, f64)>) {
    let vanilla = cfg.map.scale.vanilla(game);
    let f = |v: Option<&Value>| v.and_then(Value::as_f64);
    let total = f(sheet.get("distance_km")).unwrap_or(0.0);
    let scale = f(sheet.get("map_scale")).unwrap_or_else(|| vanilla.main_road_local_scale());
    let mut parts = Vec::new();
    let mut rest = total;
    if let Some(d) = f(sheet.get("uk").and_then(|u| u.get("distance_km"))) {
        parts.push((d, UK_SCALE));
        rest -= d;
    }
    if let Some(a) = sheet.get("adjusted_scale").filter(|a| a.is_object()) {
        if let (Some(d), Some(s)) = (f(a.get("distance_km")), f(a.get("scale"))) {
            parts.push((d, s));
            rest -= d;
        }
    }
    parts.insert(0, (rest.max(0.0), scale));
    let mut classes: Vec<(ScaleClass, f64, f64)> = Vec::new();
    let mut world = 0.0;
    for (d, s) in parts {
        let w = d / s;
        world += w;
        let c = vanilla.classify(Some(s));
        match classes.iter_mut().find(|x| x.0 == c) {
            Some(x) => {
                x.1 += d;
                x.2 += w;
            }
            None => classes.push((c, d, w)),
        }
    }
    (world, classes)
}

fn mi(km: f64) -> f64 {
    round2(km / KM_PER_MI)
}

/// The sheet as the API shows it: distances in km and mi, times as RFC 3339.
pub fn to_json(sheet: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (k, v) in sheet {
        match (k.as_str(), v) {
            ("distance_km", Value::Number(n)) => {
                let x = n.as_f64().unwrap_or(0.0);
                out.insert("distance_km".into(), json!(round3(x)));
                out.insert("distance_mi".into(), json!(mi(x)));
            }
            ("started_at_ms" | "ended_at_ms", Value::Number(n)) => {
                let key = k.trim_end_matches("_ms").to_string();
                out.insert(key, util::rfc3339(n.as_i64().unwrap_or(0)).into());
            }
            ("started_at_ms" | "ended_at_ms", _) => {
                out.insert(k.trim_end_matches("_ms").to_string(), Value::Null);
            }
            ("fuel_used_l", Value::Number(n)) => {
                out.insert(k.clone(), json!(round2(n.as_f64().unwrap_or(0.0))));
            }
            ("uk" | "adjusted_scale", Value::Object(o)) => {
                let mut sec = Map::new();
                for (sk, sv) in o {
                    if sk == "distance_km" {
                        let x = sv.as_f64().unwrap_or(0.0);
                        sec.insert("distance_km".into(), json!(round3(x)));
                        sec.insert("distance_mi".into(), json!(mi(x)));
                    } else {
                        sec.insert(sk.clone(), sv.clone());
                    }
                }
                out.insert(k.clone(), Value::Object(sec));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        crate::config::sample()
    }

    fn submission() -> Value {
        json!({"game": "ets2", "unit": "mi", "fuel_unit": "us_gal", "start_city": "Calais", "start_company": "Tradeaux",
               "end_city": "London", "end_company": "Posped", "cargo_name": "Apples", "distance": 300, "fuel_used": 40,
               "damage": 3.5, "profit": 12000, "started_at": "2026-10-14T08:00:00Z", "ended_at": "2026-10-14T09:10:00Z",
               "truck": "Scania", "truck_model": "S", "mod_names": ["Realistic Physics"],
               "uk": {"distance": 100, "entry_city": "Folkestone", "end_city": "London", "route_evidence": "evd_x"}})
    }

    #[test]
    fn submissions_convert_units_and_split_the_uk() {
        let s = parse(&submission(), Game::Ets2, true, &Map::new()).unwrap();
        assert!((s["distance_km"].as_f64().unwrap() - 300.0 * KM_PER_MI).abs() < 1e-9);
        assert!((s["fuel_used_l"].as_f64().unwrap() - 40.0 * L_PER_US_GAL).abs() < 1e-9);
        assert_eq!(s["source_city"], "Calais");
        check(&s).unwrap();
        let (w, classes) = world_km(&s, Game::Ets2, &cfg());
        let expect = (200.0 * KM_PER_MI) / 19.0 + (100.0 * KM_PER_MI) / 15.0;
        assert!((w - expect).abs() < 1e-9, "{w} vs {expect}");
        // 19 and 15 are both ETS2 road scales.
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].0, ScaleClass::Road);
    }

    #[test]
    fn defaults_and_adjusted_sections() {
        let mut b = submission();
        b["game"] = "ats".into();
        b.as_object_mut().unwrap().remove("uk");
        b["unit"] = "km".into();
        b["adjusted_scale"] = json!({"entry_city": "A", "exit_city": "B", "distance": 50, "scale": 1, "evidence": "evd_y"});
        let s = parse(&b, Game::Ats, true, &Map::new()).unwrap();
        let (w, classes) = world_km(&s, Game::Ats, &cfg());
        assert!((w - (250.0 / 20.0 + 50.0)).abs() < 1e-9);
        assert!(classes.iter().any(|c| c.0 == ScaleClass::OneToOne && c.2 == 50.0));
    }

    #[test]
    fn validation_names_every_bad_field() {
        let mut b = submission();
        b["damage"] = 120.into();
        b["uk"]["distance"] = 400.into();
        b["bonus_id"] = "bon_x".into();
        b.as_object_mut().unwrap().remove("cargo_name");
        let e = parse(&b, Game::Ets2, true, &Map::new()).unwrap_err();
        let f = &e.details["fields"];
        assert!(f.get("damage").is_some() && f.get("bonus_id").is_some() && f.get("cargo_name").is_some(), "{f}");
        let mut ok = submission();
        ok["uk"]["distance"] = 400.into();
        let s = parse(&ok, Game::Ets2, true, &Map::new()).unwrap();
        assert!(check(&s).unwrap_err().details["fields"].get("uk.distance").is_some());
        assert!(parse(&submission(), Game::Ats, true, &Map::new()).unwrap_err().details["fields"].get("uk").is_some());
    }

    #[test]
    fn edits_are_partial_and_merge_sections() {
        let base = parse(&submission(), Game::Ets2, true, &Map::new()).unwrap();
        let edit = parse(&json!({"unit": "km", "uk": {"distance": 120}, "damage": null}), Game::Ets2, false, &base).unwrap();
        assert_eq!(edit["uk"]["entry_city"], "Folkestone");
        assert_eq!(edit["uk"]["distance_km"], 120.0);
        assert_eq!(edit["damage_pct"], Value::Null);
        assert!(edit.get("distance_km").is_none());
        let e = parse(&json!({"uk": {"route_evidence": "evd_z"}}), Game::Ets2, false, &base).unwrap_err();
        assert!(e.details["fields"].get("uk.route_evidence").is_some());
        assert!(check_keys(&json!({"reason": "x", "speed": 1}), &["reason"]).is_err());
    }
}
