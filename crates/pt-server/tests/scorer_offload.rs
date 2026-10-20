//! Finished jobs are not held in memory. Since 2026-10-20 every point is in
//! the scorer's own database and only an open block per track is in memory;
//! a job brought back by a load continues exactly as on the player's client
//! (which keeps every point), and storing it again keeps every point.

mod common;

use common::seed::*;
use common::{block_on, TestHost};
use pt_core::rules::Context;
use pt_core::timeline::Profiles;
use pt_core::Game;
use pt_replay::{order_sessions, parse_lines, sample_config};
use pt_server::points::BLOCK;
use serde_json::{json, Value};

const BASE_WALL: i64 = 1_791_000_000_000;

enum End {
    Deliver,
    Cancel,
    Nothing,
}

/// One plugin session on the Chicago → Joliet tyres job (as `synthetic_job`),
/// starting `wall_s` seconds after the base wall clock, at odometer `odo` and
/// position `x`, driving `secs` at 50 mph.
fn session(sess: &str, wall_s: i64, odo: f64, x: f64, secs: f64, end: End) -> Vec<String> {
    let mut out = Vec::new();
    let (mut seq, mut sim, mut x, mut odo) = (0u64, 0u64, x, odo);
    let mut emit = |kind: &str, extra: Value, sim: u64| {
        seq += 1;
        let mut v = json!({"v": 1, "kind": kind, "sess": sess, "seq": seq, "wall_ms": BASE_WALL + wall_s * 1000 + (sim / 1000) as i64, "sim_us": sim});
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
    let sample = |x: f64, odo: f64, v: f64| {
        json!({"ch": {"truck.speed": v, "truck.odometer": odo, "truck.fuel.amount": 500.0,
            "truck.world.placement": {"position": {"x": x, "y": 0.0, "z": 0.0}}, "game.time": 1000,
            "local.scale": 20.0, "trailer.0.connected": true}})
    };
    emit("init", json!({"game_id": "ats", "game_version": {"major": 1, "minor": 7}}), 0);
    emit("config", json!({"id": "truck", "attrs": [a("id", "volvo".into())]}), 0);
    emit("config", json!({"id": "job", "attrs": job}), 0);
    emit("sample", sample(x, odo, 0.0), sim);
    let v = 50.0 * 0.44704;
    for _ in 0..(secs * 20.0) as u64 {
        sim += 50_000;
        x += v * 0.05;
        odo += v * 0.05 * 20.0 / 1000.0;
        emit("sample", sample(x, odo, v), sim);
    }
    match end {
        End::Deliver => {
            emit("gameplay", json!({"id": "job.delivered", "attrs": [a("distance.km", 8.into())]}), sim);
            emit("config", json!({"id": "job", "attrs": []}), sim);
        }
        End::Cancel => {
            emit("config", json!({"id": "job", "attrs": []}), sim);
            emit("gameplay", json!({"id": "job.cancelled", "attrs": [a("cancel.penalty", 1000.into())]}), sim);
        }
        End::Nothing => {}
    }
    sim += 50_000;
    emit("sample", sample(x, odo, 0.0), sim);
    out
}


/// The stored job's kept points (on disk).
fn stored_points(t: &TestHost) -> (String, usize) {
    let id = t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string();
    let stored = block_on(pt_server::jobs::load_stored(t, &id)).unwrap().expect("job data");
    (id, stored.job.kept.n)
}

/// What the player's client makes of the same frames (all points in memory).
fn client_view(sessions: &[Vec<String>]) -> Vec<(String, f64)> {
    let cfg = sample_config();
    let lines: Vec<String> = sessions.concat();
    let (mut frames, _) = parse_lines(&lines, &cfg);
    order_sessions(&mut frames);
    let mut p = Profiles::new(cfg.clone());
    for f in &frames {
        p.push(f);
    }
    p.into_jobs()
        .iter()
        .map(|j| (format!("{:?}", j.state).to_lowercase(), pt_core::rules::evaluate(j, &cfg, &Context::default()).distances.counted_game_km))
        .collect()
}

#[test]
fn a_delivered_job_leaves_memory_and_a_load_into_it_keeps_its_points() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    let first = session("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1", 0, 1000.0, 0.0, 60.0, End::Deliver);
    upload(&t, &token, &first, 200);
    let mut s = t.scorer(&user_id(&t), Game::Ats);
    while block_on(s.process(&t)).unwrap().more {}
    let (id, on_disk) = stored_points(&t);
    assert!(on_disk > 1000, "{on_disk}");
    let (in_memory, bytes) = s.footprint();
    assert!(in_memory <= BLOCK, "only open blocks stay in memory: {in_memory}");
    assert!(bytes > 0, "the points are on disk");
    let before = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{id}"))).body_json();

    // A save from mid-job loaded in a new session: the job stops, and storing
    // that keeps every point. A new object instance (nothing in memory) picks
    // up where the last one committed.
    let second = session("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2", 600, 1003.0, 150.0, 5.0, End::Nothing);
    upload(&t, &token, &second, 200);
    let mut s = t.scorer(&user_id(&t), Game::Ats);
    while block_on(s.process(&t)).unwrap().more {}
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}"))).body_json();
    assert!(detail.to_string().contains("stopped_after_delivery"), "{}", detail["timeline"]);
    assert_eq!(stored_points(&t), (id.clone(), on_disk));
    let after = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{id}"))).body_json();
    assert_eq!((after["outcome"].clone(), after["points"].clone()), (before["outcome"].clone(), before["points"].clone()));
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series?max_points=50"))).status, 200);
    assert!(s.footprint().0 <= BLOCK);
    assert_eq!(client_view(&[first, second])[0].0, "stopped");
}

#[test]
fn a_cancelled_job_brought_back_by_a_load_continues_as_on_the_client() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    let first = session("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb1", 0, 1000.0, 0.0, 60.0, End::Cancel);
    upload(&t, &token, &first, 200);
    let mut s = t.scorer(&user_id(&t), Game::Ats);
    while block_on(s.process(&t)).unwrap().more {}
    // A cancelled job stays with the player's tracker: no record here.
    assert!(t.q("SELECT job_id FROM jobs").is_empty());
    assert!(s.footprint().0 <= BLOCK);

    // A save from before the cancel: the job comes back from that point and
    // is delivered, exactly as the client sees it.
    let second = session("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2", 600, 1010.0, 450.0, 30.0, End::Deliver);
    upload(&t, &token, &second, 200);
    while block_on(s.process(&t)).unwrap().more {}
    let (id, _) = stored_points(&t);
    assert_eq!(t.q("SELECT job_id FROM jobs").len(), 1);
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}"))).body_json();
    assert_eq!(detail["state"], "delivered", "{}", detail["state"]);
    let timeline = detail["timeline"].to_string();
    assert!(timeline.contains("cancellation_reverted"), "{timeline}");
    let client = client_view(&[first, second]);
    assert_eq!(client.len(), 1);
    assert_eq!(client[0].0, "delivered");
    let server_km = detail["distances"]["counted_game_km"].as_f64().unwrap();
    assert!((server_km - pt_server::util::round2(client[0].1)).abs() < 1e-9, "server {server_km} vs client {}", client[0].1);
}
