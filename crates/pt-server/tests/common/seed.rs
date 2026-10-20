//! Seeding helpers shared by the API tests: publish, link a player, upload a
//! capture the way the client does, run the scorer.

use std::path::Path;

use pt_core::upload::{ManifestEntry, UploadChunk, UPLOAD_PROTOCOL};
use pt_core::Game;
use pt_server::http::{Request, Response};
use pt_server::{config, util};
use serde_json::json;
use serde_json::value::RawValue;
use serde_json::Value;

use super::{block_on, TestHost};

pub const PLAYER: &str = "123456789012345678";
pub const PLUGIN_SHA: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
pub const CLIENT_SHA: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

pub fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pt-replay/tests/fixtures/real").join(name);
    pt_replay::read_lines(&path).unwrap()
}

pub fn manifest() -> Vec<ManifestEntry> {
    vec![
        ManifestEntry {
            role: "plugin".into(),
            path: r"C:\Steam\steamapps\common\American Truck Simulator\bin\win_x64\plugins\plasmatrack.dll".into(),
            sha256: PLUGIN_SHA.into(),
        },
        ManifestEntry { role: "client_binary".into(), path: r"C:\PlasmaTrack\bin\plasmatrack-clientw.exe".into(), sha256: CLIENT_SHA.into() },
        ManifestEntry { role: "config_file".into(), path: r"C:\Users\x\plasmatrack.toml".into(), sha256: "00".repeat(32) },
    ]
}

/// Publishes the sample config and a release matching [`manifest`].
pub fn publish(t: &TestHost) {
    let r = t.call(t.admin("POST", "/api/v1/admin/config-versions").with_json(&json!({
        "toml": config::SAMPLE_TOML, "make_current": true, "reason": "Initial config"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    let r = t.call(t.admin("POST", "/api/v1/admin/releases").with_json(&json!({
        "release_version": "0.1.0", "platform": "windows", "reason": "Release 0.1.0",
        "files": [{"role": "plugin", "path_pattern": "*/bin/win_x64/plugins/plasmatrack.dll", "sha256": PLUGIN_SHA},
                  {"role": "client_binary", "path_pattern": "*/plasmatrack-clientw.exe", "sha256": CLIENT_SHA}]})));
    assert_eq!(r.status, 201, "{}", r.body_json());
}

pub fn link(t: &TestHost) -> String {
    let r = t.call(t.bot("POST", &format!("/api/v1/players/{PLAYER}/token")).with_header("idempotency-key", "link-1").with_json(&json!({"display_name": "Example Driver"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    r.body_json()["token"]["token"].as_str().unwrap().to_string()
}

/// Uploads a capture session by session, in chunks, like the client.
pub fn upload(t: &TestHost, token: &str, lines: &[String], per_chunk: usize) {
    upload_with(t, token, lines, per_chunk, manifest());
}

/// [`upload`] with a given install manifest.
pub fn upload_with(t: &TestHost, token: &str, lines: &[String], per_chunk: usize, manifest: Vec<ManifestEntry>) {
    let cfg = config::sample();
    let mut sessions: Vec<(String, Vec<&String>)> = Vec::new();
    for l in lines {
        let v: Value = serde_json::from_str(l).unwrap();
        let s = v["sess"].as_str().unwrap().to_string();
        match sessions.iter_mut().find(|(x, _)| *x == s) {
            Some((_, v)) => v.push(l),
            None => sessions.push((s, vec![l])),
        }
    }
    for (sess, frames) in sessions {
        for c in frames.chunks(per_chunk) {
            let (r, seq_to) = post_chunk(t, token, &sess, c, "0.1.0", &cfg, &manifest);
            assert_eq!(r.status, 200, "{}", r.body_json());
            assert!(r.body_json()["accepted_through"].as_u64().unwrap() >= seq_to);
        }
    }
}

/// Posts one chunk of session `sess` as client `client_version`; the response
/// and the chunk's `seq_to`.
pub fn post_chunk(
    t: &TestHost, token: &str, sess: &str, c: &[&String], client_version: &str, cfg: &pt_core::config::Config,
    manifest: &[ManifestEntry],
) -> (Response, u64) {
    let seq = |l: &String| serde_json::from_str::<Value>(l).unwrap()["seq"].as_u64().unwrap_or(0);
    let chunk = UploadChunk {
        protocol: UPLOAD_PROTOCOL,
        client_version: client_version.into(),
        sess: sess.to_string(),
        hello: None,
        seq_from: c.iter().map(|l| seq(l)).filter(|s| *s > 0).min().unwrap_or(0),
        seq_to: c.iter().map(|l| seq(l)).max().unwrap_or(0),
        frames: c.iter().map(|l| RawValue::from_string(l.to_string()).unwrap()).collect(),
        config_hash: cfg.integrity_hash(),
        config: RawValue::from_string(cfg.integrity_canonical_json()).unwrap(),
        manifest: manifest.to_vec(),
    };
    let mut req = Request::new("POST", "/api/v1/chunks")
        .with_header("authorization", &format!("Bearer {token}"))
        .with_header("content-type", "application/json")
        .with_header("content-encoding", "gzip");
    req.body = util::gzip(&serde_json::to_vec(&chunk).unwrap());
    (t.call(req), chunk.seq_to)
}

pub fn user_id(t: &TestHost) -> String {
    t.q(&format!("SELECT user_id FROM users WHERE discord_user_id = '{PLAYER}'"))[0]["user_id"].as_str().unwrap().to_string()
}

pub fn score(t: &TestHost, game: Game) -> pt_server::scorer::Report {
    let mut s = t.scorer(&user_id(t), game);
    block_on(s.process(t)).unwrap()
}

pub fn chicago(t: &TestHost) -> String {
    publish(t);
    let token = link(t);
    upload(t, &token, &fixture("ats-chicago-clean-delivery.jsonl.gz"), 400);
    let report = score(t, Game::Ats);
    assert_eq!(report.jobs_stored, 1, "{report:?}");
    t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string()
}

/// Links another Discord user (self-service) and returns their token.
pub fn link_other(t: &TestHost, discord_id: &str, name: &str) -> String {
    let r = t.call(
        t.bot("POST", &format!("/api/v1/players/{discord_id}/token"))
            .with_header("idempotency-key", &format!("link-{discord_id}"))
            .with_json(&json!({"display_name": name})),
    );
    assert_eq!(r.status, 201, "{}", r.body_json());
    r.body_json()["token"]["token"].as_str().unwrap().to_string()
}

/// A synthetic ATS session in one plugin session `sess`: a loaded job driven
/// `secs` at `mph`, then delivered (shaped like the replay simulator).
pub fn synthetic_job(sess: &str, mph: f64, secs: f64) -> Vec<String> {
    let mut out = Vec::new();
    let (mut seq, mut sim, mut x, mut odo) = (0u64, 0u64, 0.0f64, 1000.0f64);
    let mut emit = |kind: &str, extra: Value, sim: u64| {
        seq += 1;
        let mut v = json!({"v": 1, "kind": kind, "sess": sess, "seq": seq, "wall_ms": 1_791_000_000_000i64 + (sim / 1000) as i64, "sim_us": sim});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        out.push(v.to_string());
    };
    let a = |n: &str, v: Value| json!({"name": n, "index": null, "value": v});
    let job = json!([
        a("cargo.id", "tyres".into()), a("cargo", "Tyres".into()), a("source.city.id", "chicago".into()),
        a("source.city", "Chicago".into()), a("destination.city.id", "joliet".into()), a("destination.city", "Joliet".into()),
        a("source.company.id", "a".into()), a("destination.company.id", "b".into()), a("cargo.loaded", true.into()),
        a("job.market", "cargo_market".into()), a("planned_distance.km", 80.into()),
    ]);
    emit("init", json!({"game_id": "ats", "game_version": {"major": 1, "minor": 7}}), 0);
    emit("config", json!({"id": "truck", "attrs": [a("id", "volvo".into())]}), 0);
    let sample = |x: f64, odo: f64, v: f64| {
        json!({"ch": {"truck.speed": v, "truck.odometer": odo, "truck.fuel.amount": 500.0,
            "truck.world.placement": {"position": {"x": x, "y": 0.0, "z": 0.0}}, "game.time": 1000,
            "local.scale": 20.0, "trailer.0.connected": true}})
    };
    emit("sample", sample(x, odo, 0.0), sim);
    emit("config", json!({"id": "job", "attrs": job}), sim);
    let v = mph * 0.44704;
    for _ in 0..(secs * 20.0) as u64 {
        sim += 50_000;
        x += v * 0.05;
        odo += v * 0.05 * 20.0 / 1000.0;
        emit("sample", sample(x, odo, v), sim);
    }
    let km = json!([a("distance.km", ((odo - 1000.0) as u64).into())]);
    emit("gameplay", json!({"id": "job.delivered", "attrs": km}), sim);
    emit("config", json!({"id": "job", "attrs": []}), sim);
    sim += 50_000;
    emit("sample", sample(x, odo, 0.0), sim);
    out
}

/// A tiny WebP-shaped image (the server checks only the first bytes).
pub fn webp(n: u8) -> Vec<u8> {
    let mut b = b"RIFF\x20\x00\x00\x00WEBPVP8 ".to_vec();
    b.extend(std::iter::repeat_n(n, 20));
    b
}

/// Uploads an image for `discord_id`; returns its evidence id.
pub fn image(t: &TestHost, discord_id: &str, n: u8) -> String {
    let mut r = t.bot("POST", &format!("/api/v1/players/{discord_id}/evidence")).with_header("idempotency-key", &format!("img-{discord_id}-{n}"));
    r.body = webp(n);
    r = r.with_header("content-type", "image/webp");
    let resp = t.call(r);
    assert_eq!(resp.status, 201, "{}", resp.body_json());
    resp.body_json()["evidence_id"].as_str().unwrap().to_string()
}

/// An ETS2 manual submission: 380 km of which 76 km in the UK, with a custom map.
pub fn manual_body(evidence: &str, map: &str, uk_route: &str) -> Value {
    json!({
        "game": "ets2", "unit": "km", "fuel_unit": "l",
        "start_city": "Calais", "start_company": "Tradeaux", "end_city": "London", "end_company": "Posped",
        "cargo_name": "Apples", "distance": 380, "fuel_used": 120.5, "damage": 2, "profit": 15000,
        "started_at": "2026-10-14T07:00:00Z", "ended_at": "2026-10-14T08:20:00Z",
        "truck": "Scania", "truck_model": "S 730", "mod_names": ["Example Map Mod"],
        "uk": {"distance": 76, "entry_city": "Folkestone", "end_city": "London", "route_evidence": uk_route},
        "evidence": [evidence], "custom_map": map,
    })
}

/// Submits [`manual_body`] for [`PLAYER`] with three fresh images; returns the job id.
pub fn manual_job(t: &TestHost) -> String {
    let (e, m, u) = (image(t, PLAYER, 1), image(t, PLAYER, 2), image(t, PLAYER, 3));
    let r = t.call(
        t.bot("POST", &format!("/api/v1/players/{PLAYER}/manual-jobs"))
            .with_header("idempotency-key", "manual-1")
            .with_json(&manual_body(&e, &m, &u)),
    );
    assert_eq!(r.status, 201, "{}", r.body_json());
    r.body_json()["job_id"].as_str().unwrap().to_string()
}

/// Points for `world_km` under the sample config, the way the server floors them.
pub fn points_for_world_km(world_km: f64) -> f64 {
    let c = config::sample();
    c.rules.floor_points(pt_core::units::km_to_mi(world_km) * c.rules.points_per_world_mi)
}
