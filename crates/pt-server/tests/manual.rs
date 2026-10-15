//! Manual submissions, moderator edits, the all-games history and the stats
//! breakdown (PUBLIC_API §2, §3, §6; ADMIN_API §7, §14).

mod common;

use common::seed::*;
use common::{block_on, TestHost, ADMIN};
use pt_core::Game;
use pt_server::http::Request;
use pt_server::scorer::Scorer;
use serde_json::{json, Value};

fn webhook_types(t: &TestHost) -> Vec<String> {
    t.q("SELECT type FROM webhook_outbox ORDER BY created_at, event_id").iter().map(|r| r["type"].as_str().unwrap().to_string()).collect()
}

fn get(t: &TestHost, url: &str) -> pt_server::http::Response {
    t.call(Request::new("GET", url.strip_prefix("https://pt.example").unwrap()))
}

#[test]
fn manual_jobs_go_to_review_and_score_by_map_scale() {
    let t = TestHost::new();
    publish(&t);
    link(&t);

    // Images: only real image types, at most 5 MB.
    let mut bad = t.bot("POST", &format!("/api/v1/players/{PLAYER}/evidence")).with_header("idempotency-key", "bad");
    bad.body = b"not an image".to_vec();
    assert_eq!(t.call(bad).body_json()["error"]["details"]["fields"]["body"], "a WebP, PNG or JPEG image");
    let mut big = t.bot("POST", &format!("/api/v1/players/{PLAYER}/evidence")).with_header("idempotency-key", "big");
    big.body = vec![0; 5 * 1024 * 1024 + 1];
    assert_eq!(t.call(big).status, 413);

    let (e, m, u) = (image(&t, PLAYER, 1), image(&t, PLAYER, 2), image(&t, PLAYER, 3));
    let submit = |key: &str, body: &Value| {
        t.call(t.bot("POST", &format!("/api/v1/players/{PLAYER}/manual-jobs")).with_header("idempotency-key", key).with_json(body))
    };
    // Validation names the bad fields.
    let mut wrong = manual_body(&e, &m, &u);
    wrong["damage"] = 140.into();
    wrong["uk"]["distance"] = 500.into();
    let r = submit("wrong", &wrong);
    assert_eq!(r.status, 422);
    assert!(r.body_json()["error"]["details"]["fields"]["damage"].is_string(), "{}", r.body_json());

    let r = submit("ok", &manual_body(&e, &m, &u));
    assert_eq!(r.status, 201, "{}", r.body_json());
    let j = r.body_json();
    let job_id = j["job_id"].as_str().unwrap().to_string();
    assert_eq!(j["origin"], "manual");
    assert_eq!(j["outcome"], "pending_review");
    assert_eq!(j["points"], Value::Null);
    // (380 − 76) ÷ 19 + 76 ÷ 15 world km.
    let world = 304.0 / 19.0 + 76.0 / 15.0;
    assert_eq!(j["held_points"], points_for_world_km(world));
    assert_eq!(j["world_km"], (world * 100.0).round() / 100.0);
    assert_eq!(j["game_km"], 380.0);
    assert_eq!(j["cargo"]["name"], "Apples");
    assert_eq!(j["source"]["city_name"], "Calais");
    assert_eq!(j["distance_by_scale_class"]["road"]["game_km"], 380.0);
    let map = get(&t, j["custom_map"]["url"].as_str().unwrap());
    assert_eq!((map.status, map.body.clone()), (200, webp(2)));
    assert!(webhook_types(&t).contains(&"job.needs_review".to_string()));
    let hook: Value = serde_json::from_str(t.q("SELECT body FROM webhook_outbox WHERE type = 'job.needs_review'")[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(hook["data"]["origin"], "manual");
    assert_eq!(hook["data"]["reasons"][0]["code"], "manual_submission");

    // The same images cannot be used twice.
    let again = submit("again", &manual_body(&e, &m, &u));
    assert!(again.body_json()["error"]["details"]["fields"]["evidence[0]"].is_string(), "{}", again.body_json());

    // Moderators see it in the queue; the player sees their own images.
    let q = t.call(t.admin("GET", "/api/v1/admin/review-queue")).body_json();
    assert_eq!(q["items"][0]["origin"], "manual");
    let ev = t.call(t.bot("GET", &format!("/api/v1/players/{PLAYER}/jobs/{job_id}/evidence"))).body_json();
    assert_eq!(ev["items"].as_array().unwrap().len(), 3);
    assert!(ev["items"].as_array().unwrap().iter().any(|i| i["role"] == "uk_route"));
    link_other(&t, "555", "Someone Else");
    assert_eq!(t.call(t.bot("GET", &format!("/api/v1/players/555/jobs/{job_id}/evidence"))).status, 404);
    let d = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}"))).body_json();
    assert_eq!(d["submission"]["sheet"]["distance_km"], 380.0);
    assert_eq!(d["sheet"]["uk"]["entry_city"], "Folkestone");
    assert_eq!(d["evidence"].as_array().unwrap().len(), 3);
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}/route"))).status, 404);

    // Approve with a correction and the map hidden: one job.scored, by the moderator.
    let before = webhook_types(&t).len();
    let r = t.call(t.admin("POST", &format!("/api/v1/admin/jobs/{job_id}/approve")).with_json(&json!({
        "reason": "Checked the screenshots.", "expected_etag": d["etag"], "hide_map": true,
        "corrections": {"unit": "km", "distance": 400}})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    let world2 = 324.0 / 19.0 + 76.0 / 15.0;
    assert_eq!(r.body_json()["points"], points_for_world_km(world2));
    let types = webhook_types(&t);
    assert_eq!((types.len() - before, types.iter().filter(|x| *x == "job.scored").count()), (1, 1), "{types:?}");
    let p = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json();
    assert_eq!((p["outcome"].as_str(), p["custom_map"].clone()), (Some("approved"), Value::Null));
    let d = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}"))).body_json();
    assert_eq!(d["overrides"]["distance_km"], 400.0);
    assert_eq!(d["submission"]["sheet"]["distance_km"], 380.0);
    assert_eq!(d["hide_map"], true);
    let actions: Vec<&str> = d["audit"].as_array().unwrap().iter().map(|a| a["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["job.approve", "job.edit", "job.submit"]);

    // A moderator can give it a bonus afterwards; recalculation keeps it all.
    let b = t.call(t.admin("POST", "/api/v1/admin/bonuses").with_json(&json!({"game": "ets2", "label": "Apple week",
        "match": "apples", "match_kind": "exact", "kind": "multiplier", "amount": 2.0,
        "active_from": "2026-10-01T00:00:00Z", "active_to": "2026-11-01T00:00:00Z", "reason": "Event"}))).body_json();
    let r = t.call(t.admin("PATCH", &format!("/api/v1/admin/jobs/{job_id}")).with_json(&json!({"reason": "Event cargo", "bonus_id": b["bonus_id"]})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    let doubled = {
        let c = pt_server::config::sample();
        c.rules.floor_points(pt_core::units::km_to_mi(world2) * c.rules.points_per_world_mi * 2.0)
    };
    assert_eq!(r.body_json()["points"], doubled);
    t.advance(1000);
    t.call(t.admin("POST", &format!("/api/v1/admin/jobs/{job_id}/recalculate")).with_json(&json!({"reason": "Check"})));
    assert_eq!(t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json()["points"], doubled);
}

#[test]
fn moderators_edit_tracked_jobs_and_edits_survive_rescoring() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    let path = format!("/api/v1/admin/jobs/{job_id}");
    let d = t.call(t.admin("GET", &path)).body_json();
    assert_eq!(d["origin"], "tracked");
    let game_km = d["sheet"]["distance_km"].as_f64().unwrap();
    let world_km = d["distances"]["world_km"].as_f64().unwrap();
    assert_eq!(d["points"], 3.6);

    // Unknown fields and stale etags are refused.
    let r = t.call(t.admin("PATCH", &path).with_json(&json!({"reason": "Typo", "top_speed": 3})));
    assert_eq!(r.body_json()["error"]["details"]["fields"]["top_speed"], "unknown field");
    let r = t.call(t.admin("PATCH", &path).with_json(&json!({"reason": "Typo", "cargo_name": "X", "expected_etag": "\"jX-v9\""})));
    assert_eq!(r.status, 409);

    // Double the distance: the job's own city/road mix carries over.
    t.advance(1);
    let r = t.call(t.admin("PATCH", &path).with_json(&json!({"reason": "Delivery distance was cut off", "unit": "km",
        "distance": game_km * 2.0, "cargo_name": "Corrected cargo", "damage": 4, "truck": "Peterbilt"})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    let edited = r.body_json();
    assert_eq!(edited["outcome"], "valid");
    assert_eq!(edited["points"], points_for_world_km(world_km * 2.0));
    let hook: Value = serde_json::from_str(t.q("SELECT body FROM webhook_outbox WHERE type = 'job.scored' ORDER BY created_at DESC LIMIT 1")[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(hook["data"]["by"], json!({"kind": "moderator", "discord_user_id": ADMIN}));
    let p = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json();
    assert_eq!(p["cargo"]["name"], "Corrected cargo");
    let d = t.call(t.admin("GET", &path)).body_json();
    assert_eq!(d["sheet"]["truck"], "Peterbilt");
    assert_eq!(d["overrides"]["damage_pct"], 4.0);
    assert_eq!(d["distances"]["edited"], true);

    // A full replay from the raw frames keeps the edit.
    let mut s = Scorer::new(&user_id(&t), Game::Ats);
    block_on(s.reset(&t)).unwrap();
    block_on(s.process(&t)).unwrap();
    assert_eq!(t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json()["points"], edited["points"]);

    // Setting a field back to the recording's value drops its override.
    t.advance(1);
    let r = t.call(t.admin("PATCH", &path).with_json(&json!({"reason": "Undo", "unit": "km", "distance": game_km})));
    assert!(r.body_json()["overrides"].get("distance_km").is_none(), "{}", r.body_json());
    assert_eq!(r.body_json()["points"], 3.6);
}

#[test]
fn history_spans_both_games_and_stats_break_down() {
    let t = TestHost::new();
    chicago(&t);
    manual_job(&t);
    let h = t.call(t.bot("GET", &format!("/api/v1/public/players/{PLAYER}/jobs"))).body_json();
    assert_eq!(h["total"], 2);
    let games: Vec<&str> = h["items"].as_array().unwrap().iter().map(|i| i["game"].as_str().unwrap()).collect();
    assert_eq!(games, ["ets2", "ats"], "newest first");
    let only = t.call(t.bot("GET", &format!("/api/v1/public/players/{PLAYER}/jobs?outcome=pending_review"))).body_json();
    assert_eq!((only["total"].as_u64(), only["items"][0]["origin"].as_str()), (Some(1), Some("manual")));

    let p = t.call(t.bot("GET", &format!("/api/v1/public/players/{PLAYER}"))).body_json();
    let ats = &p["stats"]["ats"];
    assert_eq!(ats["outcomes"]["valid"], 1);
    assert_eq!(ats["outcomes"]["rejected"], 0);
    let classes: f64 = ["city", "road", "one_to_one", "nonstandard", "unknown"].iter().map(|c| ats["distance_by_scale_class"][c]["world_km"].as_f64().unwrap()).sum();
    assert!((classes - ats["world_km"].as_f64().unwrap()).abs() < 0.05, "{classes} vs {}", ats["world_km"]);
    assert_eq!(p["stats"]["ets2"]["outcomes"]["pending_review"], 1);
    assert_eq!(p["stats"]["ets2"]["distance_by_scale_class"]["road"]["world_km"], 0.0, "pending jobs do not count");
}

#[test]
fn unused_images_are_deleted_after_a_day() {
    let t = TestHost::new();
    link(&t);
    let id = image(&t, PLAYER, 9);
    block_on(pt_server::manage::cron(&t)).unwrap();
    assert_eq!(t.q("SELECT COUNT(*) AS n FROM evidence")[0]["n"], 1);
    t.advance(25 * 3_600_000);
    block_on(pt_server::manage::cron(&t)).unwrap();
    assert_eq!(t.q("SELECT COUNT(*) AS n FROM evidence")[0]["n"], 0);
    assert!(!t.blobs.borrow().keys().any(|k| k.contains(&id)));
}
