//! Typed view of the plugin's raw frames (docs/WIRE.md), as the timeline needs
//! them. Parsing is lenient: unknown kinds and channels are kept or ignored, never
//! fatal, so newer plugins and undocumented game data do not break replay.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Multiplayer;
use crate::game::Game;

/// One plugin message, in the order the plugin produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub sess: String,
    pub seq: u64,
    pub wall_ms: i64,
    /// `paused_simulation_time` of the frame (µs); absent on sender-thread messages.
    pub sim_us: Option<u64>,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Body {
    Init { game: Option<Game>, game_version: (u32, u32) },
    Config { id: String, attrs: Vec<Attr> },
    Gameplay { id: String, attrs: Vec<Attr> },
    Paused,
    Started,
    TimerRestart,
    Sample(Sample),
    Scale { local_scale: Option<f64> },
    /// Module list, raw (capture mode, local client) or redacted (uploads).
    /// `mods`: the kinds of `[[multiplayer.mods]]` whose module names it has.
    Modules {
        truckersmp: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mods: Vec<String>,
    },
    /// TruckersMP client SDK state (plugin >= this change): authoritative for
    /// "on a TruckersMP server" once seen in a session. `account_id` is the
    /// player's TruckersMP account (0 = unknown); uploaded for the bot to link
    /// TruckersMP profiles (PLAN §11, decision 24).
    Truckersmp { loaded: bool, connected: bool, server_id: u64, account_id: u64 },
    Shutdown,
    /// Client-inserted: the active game profile changed (PLAN §6b).
    Profile { key: String },
    Other(String),
}

/// A config or gameplay attribute: `{name, index, type, value}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attr {
    pub name: String,
    pub index: Option<u32>,
    pub value: Value,
}

/// Channel values of one sample (SDK units). `None` = the game reported no value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub speed_mps: Option<f64>,
    pub odometer_km: Option<f64>,
    pub fuel_l: Option<f64>,
    /// World position in metres of the 3D world (`truck.world.placement`).
    pub pos: Option<[f64; 3]>,
    /// In-game minutes (`game.time`).
    pub game_time_min: Option<u32>,
    pub local_scale: Option<f64>,
    pub mp_time_offset: Option<i64>,
    /// First trailer hitched (`trailer.0.connected`); absent in older captures.
    pub trailer_connected: Option<bool>,
    /// Cargo damage 0-1 (`job.cargo.damage`), for the job graphs.
    #[serde(default)]
    pub cargo_damage: Option<f64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TraceError {
    #[error("not a JSON object: {0}")]
    Json(String),
    #[error("missing envelope field {0}")]
    Missing(&'static str),
}

pub fn attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Value> {
    attrs.iter().find(|a| a.name == name && a.index.unwrap_or(0) == 0).map(|a| &a.value)
}

fn parse_attrs(v: &Value) -> Vec<Attr> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| Attr {
                    name: x["name"].as_str().unwrap_or_default().to_string(),
                    index: x["index"].as_u64().map(|i| i as u32),
                    value: x["value"].clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_sample(ch: &Value) -> Sample {
    let f = |k: &str| ch.get(k).and_then(Value::as_f64);
    let pos = ch
        .get("truck.world.placement")
        .and_then(|p| p.get("position"))
        .and_then(|p| Some([p["x"].as_f64()?, p["y"].as_f64()?, p["z"].as_f64()?]));
    Sample {
        speed_mps: f("truck.speed"),
        odometer_km: f("truck.odometer"),
        fuel_l: f("truck.fuel.amount"),
        pos,
        game_time_min: ch.get("game.time").and_then(Value::as_u64).map(|v| v as u32),
        local_scale: f("local.scale"),
        mp_time_offset: ch.get("multiplayer.time.offset").and_then(Value::as_i64),
        trailer_connected: ch.get("trailer.0.connected").and_then(Value::as_bool),
        cargo_damage: f("job.cargo.damage"),
    }
}

/// Parses one frame. `mp` names the modules that mean multiplayer, for raw
/// module lists and for the mod matches a redacted one keeps.
pub fn parse_frame(json: &str, mp: &Multiplayer) -> Result<Frame, TraceError> {
    let v: Value = serde_json::from_str(json).map_err(|e| TraceError::Json(e.to_string()))?;
    if !v.is_object() {
        return Err(TraceError::Json("not an object".into()));
    }
    let kind = v["kind"].as_str().ok_or(TraceError::Missing("kind"))?;
    let sess = v["sess"].as_str().ok_or(TraceError::Missing("sess"))?.to_string();
    let seq = v["seq"].as_u64().unwrap_or(0);
    let wall_ms = v["wall_ms"].as_i64().unwrap_or(0);
    let sim_us = v["sim_us"].as_u64();
    let body = match kind {
        "init" => {
            let gv = &v["game_version"];
            Body::Init {
                game: v["game_id"].as_str().and_then(Game::from_sdk_game_id),
                game_version: (
                    gv["major"].as_u64().unwrap_or(0) as u32,
                    gv["minor"].as_u64().unwrap_or(0) as u32,
                ),
            }
        }
        "config" => Body::Config {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            attrs: parse_attrs(&v["attrs"]),
        },
        "gameplay" => Body::Gameplay {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            attrs: parse_attrs(&v["attrs"]),
        },
        "paused" => Body::Paused,
        "started" => Body::Started,
        "timer_restart" => Body::TimerRestart,
        "sample" => Body::Sample(parse_sample(&v["ch"])),
        "scale" => Body::Scale { local_scale: v["local_scale"].as_f64() },
        "modules" => {
            let wanted: Vec<String> = mp.truckersmp_module_names.iter().map(|n| n.to_ascii_lowercase()).collect();
            let raw_hit = v["modules"]
                .as_array()
                .is_some_and(|m| m.iter().filter_map(Value::as_str).any(|n| wanted.contains(&n.to_ascii_lowercase())));
            let redacted_hit = v["truckersmp_matches"].as_array().is_some_and(|m| !m.is_empty());
            let names: Vec<&str> = [&v["modules"], &v["mod_matches"]]
                .into_iter()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            Body::Modules { truckersmp: raw_hit || redacted_hit, mods: mp.mods_matching(&names) }
        }
        "truckersmp" => Body::Truckersmp {
            loaded: v["loaded"].as_bool().unwrap_or(false),
            connected: v["connected"].as_bool().unwrap_or(false),
            server_id: v["server_id"].as_u64().unwrap_or(0),
            account_id: v["account_id"].as_u64().unwrap_or(0),
        },
        "shutdown" => Body::Shutdown,
        "profile" => Body::Profile { key: v["profile_key"].as_str().unwrap_or("unknown").to_string() },
        other => Body::Other(other.to_string()),
    };
    Ok(Frame { sess, seq, wall_ms, sim_us, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESS: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_a_real_sample_line() {
        // Shape copied from the first ATS capture (docs/PLAN.md §3).
        let line = format!(
            r#"{{"v":1,"kind":"sample","sess":"{SESS}","seq":390,"wall_ms":1790600226175,"frame":20851,"sim_us":33332,"simulation_us":347502766,"render_us":347472939,"ch":{{"truck.speed":-0.00019482193,"truck.odometer":12844.921,"truck.fuel.amount":642.4152,"truck.world.placement":{{"position":{{"x":-103988.59376907349,"y":11.977951049804688,"z":-66894.78128051758}},"orientation":{{"heading":0.92525786,"pitch":2.5044026e-06,"roll":-1.3015305e-05}}}},"game.time":10401482,"local.scale":3,"multiplayer.time.offset":0,"job.cargo.damage":null,"trailer.0.connected":true}},"stale":[]}}"#
        );
        let f = parse_frame(&line, &mp(&[])).unwrap();
        assert_eq!((f.seq, f.sim_us), (390, Some(33332)));
        let Body::Sample(s) = f.body else { panic!("not a sample") };
        assert_eq!(s.odometer_km, Some(12844.921));
        assert_eq!(s.local_scale, Some(3.0));
        assert_eq!(s.game_time_min, Some(10401482));
        assert_eq!(s.trailer_connected, Some(true));
        assert_eq!(s.pos.unwrap()[0], -103988.59376907349);
    }

    #[test]
    fn parses_config_gameplay_and_modules() {
        let cfg = format!(
            r#"{{"v":1,"kind":"config","sess":"{SESS}","seq":5,"wall_ms":1,"id":"job","attrs":[{{"name":"cargo.id","index":null,"type":"string","value":"tyres"}},{{"name":"planned_distance.km","index":null,"type":"u32","value":235}}]}}"#
        );
        let Body::Config { id, attrs } = parse_frame(&cfg, &mp(&[])).unwrap().body else { panic!() };
        assert_eq!(id, "job");
        assert_eq!(attr(&attrs, "planned_distance.km").and_then(Value::as_u64), Some(235));
        let raw = format!(r#"{{"v":1,"kind":"modules","sess":"{SESS}","seq":3,"modules":["KERNEL32.DLL","core_atsmp.dll"]}}"#);
        let tmp = mp(&["core_atsmp.dll"]);
        assert_eq!(parse_frame(&raw, &tmp).unwrap().body, Body::Modules { truckersmp: true, mods: vec![] });
        assert_eq!(parse_frame(&raw, &mp(&[])).unwrap().body, Body::Modules { truckersmp: false, mods: vec![] });
        let red = format!(r#"{{"v":1,"kind":"modules","sess":"{SESS}","seq":3,"module_count":2,"truckersmp_matches":["core_atsmp.dll"],"redacted":true}}"#);
        assert_eq!(parse_frame(&red, &mp(&[])).unwrap().body, Body::Modules { truckersmp: true, mods: vec![] });
    }

    #[test]
    fn modules_name_a_multiplayer_mod_raw_or_redacted() {
        let cfg = haulmp();
        let raw = format!(r#"{{"v":1,"kind":"modules","sess":"{SESS}","seq":3,"modules":["KERNEL32.DLL","HaulMP_Stub.dll"]}}"#);
        assert_eq!(parse_frame(&raw, &cfg).unwrap().body, Body::Modules { truckersmp: false, mods: vec!["haulmp".into()] });
        // The redacted list keeps the matched names; the kind is looked up again.
        let red = format!(r#"{{"v":1,"kind":"modules","sess":"{SESS}","seq":3,"module_count":2,"truckersmp_matches":[],"mod_matches":["haulmp-telemetry.dll"],"redacted":true}}"#);
        assert_eq!(parse_frame(&red, &cfg).unwrap().body, Body::Modules { truckersmp: false, mods: vec!["haulmp".into()] });
        // A config without the mod does not know the name.
        assert_eq!(parse_frame(&red, &mp(&[])).unwrap().body, Body::Modules { truckersmp: false, mods: vec![] });
    }

    fn mp(truckersmp: &[&str]) -> Multiplayer {
        Multiplayer { truckersmp_module_names: truckersmp.iter().map(|s| s.to_string()).collect(), mods: vec![] }
    }

    fn haulmp() -> Multiplayer {
        Multiplayer {
            truckersmp_module_names: vec![],
            mods: vec![crate::config::MultiplayerMod {
                kind: "haulmp".into(),
                name: "HaulMP".into(),
                module_names: vec!["haulmp_stub.dll".into(), "haulmp-telemetry.dll".into()],
                game_min_per_real_min: None,
                singleplayer_division_discard: true,
            }],
        }
    }

    #[test]
    fn parses_truckersmp_state_with_account() {
        let line = format!(r#"{{"v":1,"kind":"truckersmp","sess":"{SESS}","seq":7,"loaded":true,"connected":true,"server_id":42,"account_id":5557777}}"#);
        assert_eq!(
            parse_frame(&line, &mp(&[])).unwrap().body,
            Body::Truckersmp { loaded: true, connected: true, server_id: 42, account_id: 5557777 }
        );
    }
}
