//! Finished jobs are not held in memory (owner, 2026-10-16): once stored, only
//! their last point stays in the scorer; the rest is on disk. A job brought
//! back by a load continues from that point with a gap, and storing it again
//! puts the points on disk and the new ones together.

mod common;

use common::seed::*;
use common::{block_on, TestHost};
use pt_core::Game;
use pt_server::scorer::Scorer;
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

/// Every job in the scorer's saved snapshot: (state, points in memory, offloaded).
fn snapshot_jobs(t: &TestHost, s: &mut Scorer) -> Vec<(String, usize, bool)> {
    block_on(s.save(t)).unwrap();
    let key = t.blobs.borrow().keys().find(|k| k.starts_with("snap/")).cloned().unwrap();
    let snap: Value = pt_server::util::gunzip_json(&t.blobs.borrow()[&key]).unwrap();
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<(String, usize, bool)>) {
        match v {
            Value::Object(o) if o.contains_key("kept") && o.contains_key("events") => {
                out.push((o["state"].as_str().unwrap_or("").to_string(), o["kept"].as_array().map_or(0, Vec::len), o["offloaded"] == true));
            }
            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(&snap, &mut out);
    out
}

/// The stored job's points (on disk).
fn stored_points(t: &TestHost) -> (String, usize) {
    let id = t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string();
    let blob = t.blobs.borrow()[&pt_server::jobs::blob_key(&id)].clone();
    (id, pt_server::jobs::decode_blob(&blob).unwrap().0.kept.len())
}

#[test]
fn a_delivered_job_leaves_memory_and_a_load_into_it_keeps_its_points() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    upload(&t, &token, &session("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1", 0, 1000.0, 0.0, 60.0, End::Deliver), 200);
    let mut s = Scorer::new(&user_id(&t), Game::Ats);
    while block_on(s.process(&t)).unwrap().more {}
    let (id, on_disk) = stored_points(&t);
    assert!(on_disk > 1000, "{on_disk}");
    assert_eq!(snapshot_jobs(&t, &mut s), vec![("delivered".to_string(), 1, true)], "only the last point stays in memory");
    let before = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{id}"))).body_json();

    // A save from mid-job loaded in a new session: the job stops, and storing
    // that keeps every point.
    upload(&t, &token, &session("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2", 600, 1003.0, 150.0, 5.0, End::Nothing), 200);
    while block_on(s.process(&t)).unwrap().more {}
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}"))).body_json();
    assert!(detail.to_string().contains("stopped_after_delivery"), "{}", detail["timeline"]);
    assert_eq!(stored_points(&t), (id.clone(), on_disk));
    let after = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{id}"))).body_json();
    assert_eq!((after["outcome"].clone(), after["points"].clone()), (before["outcome"].clone(), before["points"].clone()));
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series?max_points=50"))).status, 200);
    let jobs = snapshot_jobs(&t, &mut s);
    assert!(jobs.iter().all(|(_, n, off)| *n <= 1 && *off), "{jobs:?}");
}

#[test]
fn a_cancelled_job_brought_back_by_a_load_continues_with_a_gap() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    upload(&t, &token, &session("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb1", 0, 1000.0, 0.0, 60.0, End::Cancel), 200);
    let mut s = Scorer::new(&user_id(&t), Game::Ats);
    while block_on(s.process(&t)).unwrap().more {}
    // A cancelled job stays with the player's tracker: no record here, and
    // only its last point in memory.
    assert!(t.q("SELECT job_id FROM jobs").is_empty());
    assert_eq!(snapshot_jobs(&t, &mut s), vec![("cancelled".to_string(), 1, true)]);

    // A save from before the cancel: the job comes back and is delivered,
    // with a gap where its earlier part was.
    upload(&t, &token, &session("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2", 600, 1010.0, 450.0, 30.0, End::Deliver), 200);
    while block_on(s.process(&t)).unwrap().more {}
    let (id, on_disk) = stored_points(&t);
    assert_eq!(t.q("SELECT job_id FROM jobs").len(), 1);
    assert!((600..700).contains(&on_disk), "the last point before the cancel and the points after the load: {on_disk}");
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}"))).body_json();
    assert_eq!(detail["state"], "delivered", "{}", detail["state"]);
    let reasons: Vec<&str> = detail["reasons"].as_array().unwrap().iter().filter_map(|r| r["code"].as_str()).collect();
    assert!(reasons.contains(&"gap"), "{reasons:?}");
    assert_eq!(detail["outcome"], "pending_review");
}
