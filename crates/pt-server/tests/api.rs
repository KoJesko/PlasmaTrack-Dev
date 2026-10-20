//! End to end through the HTTP API: a player links, uploads a real capture the
//! way the client does, the scorer runs, and the bot reads and moderates.

mod common;


use common::seed::*;
use common::{block_on, TestHost, ADMIN, BOT_TOKEN, READ_TOKEN, WEBHOOK_SECRET};
use pt_core::Game;
use pt_server::http::Request;
use pt_server::{util, webhooks};
use serde_json::{json, Value};


#[test]
fn upload_score_and_read_a_real_delivery() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    assert_eq!(t.pokes.borrow().len(), t.q("SELECT COUNT(*) AS n FROM raw_chunks")[0]["n"].as_u64().unwrap() as usize);

    // Public job detail: the same result as the replay regression (3.6 points).
    let r = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{}", job_id.to_lowercase())));
    assert_eq!(r.status, 200);
    let j = r.body_json();
    assert_eq!(j["outcome"], "valid", "{j}");
    assert_eq!(j["points"], 3.6);
    assert_eq!(j["game"], "ats");
    assert_eq!(j["player"]["discord_user_id"], PLAYER);
    assert_eq!(j["multiplayer"]["kind"], "singleplayer");
    assert_eq!(j["distance_by_scale_class"].as_object().unwrap().len(), 5);
    assert!(j["distance_by_scale_class"]["road"]["world_mi"].as_f64().unwrap() > 0.0);
    assert!(j["source"]["city_name"].as_str().is_some_and(|s| !s.is_empty()), "{j}");
    assert!(j["delivered_at"].as_str().unwrap().ends_with('Z'));
    for hidden in ["reasons", "detail", "segments", "timeline"] {
        assert!(j.get(hidden).is_none(), "{hidden} must stay admin-only");
    }

    // The read token reads too; lists and stats.
    let r = t.call(Request::new("GET", &format!("/api/v1/public/ats/players/{PLAYER}/jobs")).with_header("authorization", &format!("Bearer {READ_TOKEN}")));
    assert_eq!(r.status, 200);
    assert_eq!(r.body_json()["total"], 1);
    let p = t.call(t.bot("GET", &format!("/api/v1/public/players/{PLAYER}"))).body_json();
    assert_eq!(p["stats"]["ats"]["points"], 3.6);
    assert_eq!(p["stats"]["ets2"]["job_count"], 0);
    let lb = t.call(t.bot("GET", "/api/v1/public/ats/leaderboard?period=month")).body_json();
    assert_eq!(lb["items"][0]["rank"], 1);
    assert_eq!(lb["items"][0]["points"], 3.6);
    let lb = t.call(t.bot("GET", "/api/v1/public/ats/leaderboard?period=week&at=2020-W01")).body_json();
    assert_eq!(lb["total"], 0, "another week is empty");
    assert_eq!(t.call(t.bot("GET", "/api/v1/public/ats/leaderboard?division=singleplayer")).body_json()["total"], 0);

    // A valid result emits job.scored; delivering it signs it for the bot.
    let ev = t.q("SELECT event_id, type FROM webhook_outbox");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["type"], "job.scored");
    let id = ev[0]["event_id"].as_str().unwrap();
    assert_eq!(block_on(webhooks::deliver(&t, id)).unwrap(), webhooks::Delivery::Done);
    let posts = t.posts.borrow();
    let (url, headers, body) = &posts[0];
    assert_eq!(url, "https://bot.example/webhooks/plasmatrack");
    let sig = &headers.iter().find(|(k, _)| k == "x-plasmatrack-signature").unwrap().1;
    assert!(webhooks::verify(WEBHOOK_SECRET, sig, body, t.now.get() / 1000));
    assert!(!webhooks::verify("other", sig, body, t.now.get() / 1000));
    let b: Value = serde_json::from_slice(body).unwrap();
    assert_eq!((b["type"].as_str(), b["data"]["job_id"].as_str(), b["data"]["by"]["kind"].as_str()), (Some("job.scored"), Some(job_id.as_str()), Some("rules")));
}

#[test]
fn moderators_decide_with_etags_and_idempotency() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    let path = format!("/api/v1/admin/jobs/{job_id}");
    let d = t.call(t.admin("GET", &path)).body_json();
    let etag = d["etag"].as_str().unwrap().to_string();
    assert_eq!(d["start_kind"], "tracked");
    assert_eq!(d["integrity"]["manifest_result"], "ok", "{}", d["integrity"]);
    assert!(d["timeline"].as_array().unwrap().iter().any(|e| e["event"] == "delivered"));

    // Reject, with the etag; the same key again replays; a new body conflicts.
    let reject = |key: &str, body: Value| t.call(t.bot("POST", &format!("{path}/reject")).with_header("x-plasmatrack-actor", ADMIN).with_header("idempotency-key", key).with_json(&body));
    let r = reject("dec-1", json!({"reason": "Testing a rejection", "expected_etag": etag}));
    assert_eq!(r.status, 200, "{}", r.body_json());
    assert_eq!(r.body_json()["outcome"], "rejected");
    assert_eq!(r.body_json()["points"], 0.0);
    assert_eq!(reject("dec-1", json!({"reason": "Testing a rejection", "expected_etag": etag})).body_json(), r.body_json());
    assert_eq!(reject("dec-1", json!({"reason": "Something else"})).body_json()["error"]["code"], "idempotency_mismatch");
    // The old etag is stale now.
    let r = t.call(t.admin("POST", &format!("{path}/approve")).with_json(&json!({"reason": "Fine after all", "expected_etag": etag})));
    assert_eq!((r.status, r.body_json()["error"]["code"].as_str()), (409, Some("conflict")));
    let r = t.call(t.admin("POST", &format!("{path}/approve")).with_json(&json!({"reason": "Fine after all"})));
    assert_eq!(r.body_json()["outcome"], "approved");
    assert_eq!(r.body_json()["points"], 3.6);
    let r = t.call(t.admin("POST", &format!("{path}/restore")).with_json(&json!({"reason": "Not voided"})));
    assert_eq!((r.status, r.body_json()["error"]["code"].as_str()), (409, Some("invalid_transition")));
    t.advance(1);
    let r = t.call(t.admin("POST", &format!("{path}/void")).with_json(&json!({"reason": "Void it"})));
    assert_eq!(r.body_json()["outcome"], "voided");
    t.advance(1);
    let r = t.call(t.admin("POST", &format!("{path}/restore")).with_json(&json!({"reason": "Bring it back"})));
    assert_eq!(r.body_json()["outcome"], "approved", "restore returns the outcome before the void");

    // Decisions stick through a recalculation.
    t.advance(1);
    let r = t.call(t.admin("POST", &format!("{path}/recalculate")).with_json(&json!({"reason": "Check stickiness"})));
    assert_eq!(r.body_json()["after"]["outcome"], "approved", "{}", r.body_json());

    // Missing reason and missing idempotency key.
    let r = t.call(t.admin("POST", &format!("{path}/void")).with_json(&json!({})));
    assert_eq!(r.body_json()["error"]["details"]["fields"]["reason"], "required");
    let r = t.call(t.bot("POST", &format!("{path}/void")).with_header("x-plasmatrack-actor", ADMIN).with_json(&json!({"reason": "abc"})));
    assert_eq!(r.status, 400);

    // The audit log has every decision, newest first.
    let a = t.call(t.admin("GET", &format!("/api/v1/admin/audit?job_id={job_id}"))).body_json();
    let actions: Vec<&str> = a["items"].as_array().unwrap().iter().map(|i| i["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["job.recalculate", "job.restore", "job.void", "job.approve", "job.reject"]);
    assert_eq!(a["items"][0]["actor"]["discord_user_id"], ADMIN);
}

#[test]
fn auth_is_enforced() {
    let t = TestHost::new();
    let r = t.call(Request::new("GET", "/api/v1/admin/review-queue"));
    assert_eq!(r.status, 401);
    let read = |m: &str, p: &str| Request::new(m, p).with_header("authorization", &format!("Bearer {READ_TOKEN}"));
    assert_eq!(t.call(read("GET", "/api/v1/admin/review-queue").with_header("x-plasmatrack-actor", ADMIN)).status, 403);
    assert_eq!(t.call(read("POST", &format!("/api/v1/players/{PLAYER}/token")).with_header("idempotency-key", "x")).status, 403);
    let r = t.call(t.bot("GET", "/api/v1/admin/review-queue").with_header("x-plasmatrack-actor", "555"));
    assert_eq!(r.status, 403, "not an admin");
    assert_eq!(t.call(t.admin("GET", "/api/v1/admin/review-queue")).status, 200, "a bootstrap admin");
    // A player token never reads the bot API, and the bot token never uploads.
    let token = link(&t);
    let r = t.call(Request::new("GET", "/api/v1/admin/review-queue").with_header("authorization", &format!("Bearer {token}")));
    assert_eq!(r.status, 401);
    let r = t.call(Request::new("POST", "/api/v1/chunks").with_header("authorization", &format!("Bearer {BOT_TOKEN}")));
    assert_eq!(r.status, 401);
    // A revoked token stops uploads.
    let r = t.call(t.bot("DELETE", &format!("/api/v1/players/{PLAYER}/token")).with_header("idempotency-key", "rv"));
    assert_eq!(r.status, 200);
    let r = t.call(Request::new("POST", "/api/v1/chunks").with_header("authorization", &format!("Bearer {token}")));
    assert_eq!(r.status, 401);
}

#[test]
fn token_replays_never_show_the_plaintext_again() {
    let t = TestHost::new();
    let req = || t.bot("POST", &format!("/api/v1/players/{PLAYER}/token")).with_header("idempotency-key", "same").with_json(&json!({"display_name": "Example Driver"}));
    let first = t.call(req());
    assert_eq!(first.status, 201);
    assert!(first.body_json()["token"]["token"].as_str().unwrap().starts_with("pt_live_"));
    assert_eq!(first.body_json()["user"]["created"], true);
    let again = t.call(req());
    assert_eq!(again.status, 200);
    assert_eq!(again.body_json()["token"]["token"], Value::Null);
    assert_eq!(again.body_json()["details"]["replayed"], true);
    let meta = t.call(t.bot("GET", &format!("/api/v1/players/{PLAYER}/token"))).body_json();
    assert_eq!(meta["token_id"], first.body_json()["token"]["token_id"]);
    assert!(meta.get("token").is_none());
    // Unlinked players: 200 with linked false.
    assert_eq!(t.call(t.bot("GET", "/api/v1/public/players/42")).body_json(), json!({"linked": false, "discord_user_id": "42"}));
}

#[test]
fn without_a_published_release_jobs_are_discarded() {
    let t = TestHost::new();
    let token = link(&t);
    upload(&t, &token, &fixture("ats-chicago-clean-delivery.jsonl.gz"), 1000);
    score(&t, Game::Ats);
    let row = &t.q("SELECT outcome, points FROM jobs")[0];
    assert_eq!(row["outcome"], "discarded");
    assert_eq!(t.q("SELECT type FROM webhook_outbox")[0]["type"], "job.discarded");
    let id = t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string();
    let j = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{id}"))).body_json();
    assert_eq!(j["public_notes"][0]["code"], "discarded_integrity");
    assert_eq!(j["world_km"], 0.0);
    let r = t.call(t.admin("POST", &format!("/api/v1/admin/jobs/{id}/approve")).with_json(&json!({"reason": "Try to approve"})));
    assert_eq!(r.body_json()["error"]["code"], "invalid_transition", "discarded jobs are never approvable");
}

#[test]
fn scoring_in_pieces_with_a_restart_matches_one_pass() {
    // One pass.
    let a = TestHost::new();
    chicago(&a);
    let one: Value = a.q("SELECT summary, points FROM jobs")[0].clone();

    // The same upload, scored after every chunk; the scorer is restarted from its
    // own database half way (a Durable Object eviction: no save step, every
    // chunk is committed as it is processed).
    let b = TestHost::new();
    publish(&b);
    let token = link(&b);
    let lines = fixture("ats-chicago-clean-delivery.jsonl.gz");
    let mid = lines.len() / 2;
    upload(&b, &token, &lines[..mid], 300);
    let mut s = b.scorer(&user_id(&b), Game::Ats);
    block_on(s.process(&b)).unwrap();
    let mut s = b.scorer(&user_id(&b), Game::Ats);
    upload(&b, &token, &lines[mid..], 300);
    let report = block_on(s.process(&b)).unwrap();
    assert!(!report.rebuilt);
    assert_eq!(b.q("SELECT summary, points FROM jobs")[0], one);
}

#[test]
fn resent_chunks_are_stored_once() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    let lines = fixture("ats-chicago-clean-delivery.jsonl.gz");
    upload(&t, &token, &lines[..500], 250);
    upload(&t, &token, &lines[..500], 250); // the client never got the acks
    upload(&t, &token, &lines[..750], 250); // and then a bigger chunk overlapping
    let stored: u64 = t.q("SELECT SUM(frames) AS n FROM raw_chunks")[0]["n"].as_u64().unwrap();
    assert_eq!(stored, 750);
}

#[test]
fn clients_below_the_minimum_version_are_refused_until_they_update() {
    let mut t = TestHost::new();
    t.settings.min_client_version = Some((0, 2, 4));
    publish(&t);
    let token = link(&t);
    let lines = fixture("ats-chicago-clean-delivery.jsonl.gz");
    let sess = serde_json::from_str::<Value>(&lines[0]).unwrap()["sess"].as_str().unwrap().to_string();
    let c: Vec<&String> = lines[..250].iter().collect();
    let (cfg, m) = (pt_server::config::sample(), manifest());
    for old in ["0.2.3", "0.1.0", "garbage"] {
        let (r, _) = post_chunk(&t, &token, &sess, &c, old, &cfg, &m);
        assert_eq!(r.status, 426, "{old}");
        assert_eq!(r.body_json()["error"]["code"], "client_too_old");
        assert_eq!(r.body_json()["error"]["details"]["min_client_version"], "0.2.4");
    }
    assert_eq!(t.q("SELECT COUNT(*) AS n FROM raw_chunks")[0]["n"], 0);
    // Updated, the same data goes in (the client kept it).
    for new in ["0.2.4", "0.3.0"] {
        let (r, _) = post_chunk(&t, &token, &sess, &c, new, &cfg, &m);
        assert_eq!(r.status, 200, "{new}: {}", r.body_json());
    }
    assert_eq!(t.q("SELECT SUM(frames) AS n FROM raw_chunks")[0]["n"], 250);
}

#[test]
fn bonuses_apply_on_recalculation_and_are_listed() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    let cargo = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json()["cargo"]["id"].as_str().unwrap().to_string();
    let r = t.call(t.admin("POST", "/api/v1/admin/bonuses").with_json(&json!({
        "game": "ats", "label": "Double week", "match": cargo.to_uppercase(), "match_kind": "exact", "kind": "multiplier", "amount": 2.0,
        "active_from": "2026-01-01T00:00:00Z", "active_to": "2027-01-01T00:00:00Z", "reason": "Event"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    assert_eq!(r.body_json()["status"], "active");
    t.advance(1);
    let bad = t.call(t.admin("POST", "/api/v1/admin/bonuses").with_json(&json!({"game": "ats", "kind": "multiplier", "amount": 0.5, "reason": "Bad"})));
    assert_eq!(bad.status, 422);
    assert!(bad.body_json()["error"]["details"]["fields"]["amount"].is_string());

    let r = t.call(t.admin("POST", "/api/v1/admin/recalculations").with_json(&json!({"reason": "Bonus added", "dry_run": true})));
    assert_eq!(r.body_json()["affected_jobs"], 1);
    let r = t.call(t.admin("POST", "/api/v1/admin/recalculations").with_json(&json!({"reason": "Bonus added"})));
    assert_eq!(r.status, 202);
    let op = r.body_json()["operation_id"].as_str().unwrap().to_string();
    let msg = t.ops_queued.borrow()[0].clone();
    assert!(block_on(pt_server::manage::run_op(&t, &msg)).unwrap().is_none(), "one batch is enough");
    let o = t.call(t.admin("GET", &format!("/api/v1/admin/operations/{op}"))).body_json();
    assert_eq!((o["status"].as_str(), o["progress"]["done"].as_i64()), (Some("succeeded"), Some(1)));
    let j = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}"))).body_json();
    assert_eq!(j["points"], 7.3, "2 × 3.66 raw points, floored");
    assert_eq!(j["bonus"]["label"], "Double week");
    let kinds: Vec<String> = t.q("SELECT type FROM webhook_outbox ORDER BY created_at").iter().map(|r| r["type"].as_str().unwrap().to_string()).collect();
    assert!(kinds.contains(&"operation.completed".to_string()), "{kinds:?}");
}

#[test]
fn series_and_signed_trace_download() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    let s = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}/series?max_points=200"))).body_json();
    let pts = s["points"].as_array().unwrap();
    assert!(!pts.is_empty() && pts.len() <= 200);
    assert!(pts.iter().any(|p| p["speed_mph_max"].as_f64().unwrap() > 30.0));
    let tr = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}/trace"))).body_json();
    assert!(tr["frames"].as_i64().unwrap() > 1000, "{tr}");
    let url = tr["url"].as_str().unwrap().strip_prefix("https://pt.example").unwrap().to_string();
    let r = t.call(Request::new("GET", &url));
    assert_eq!(r.status, 200);
    let text = String::from_utf8(util::gunzip(&r.body, usize::MAX).unwrap()).unwrap();
    assert!(text.lines().all(|l| l.contains("\"pt_status\"")));
    assert!(text.contains("\"pt_status\":\"kept\""));
    assert_eq!(t.call(Request::new("GET", &url.replace("sig=", "sig=0"))).status, 403);
    t.advance(16 * 60_000);
    assert_eq!(t.call(Request::new("GET", &url)).status, 403, "expired");
}

#[test]
fn routes_land_on_the_real_map() {
    let t = TestHost::new();
    let job_id = chicago(&t);
    let r = t.call(t.bot("GET", &format!("/api/v1/public/jobs/{job_id}/route")));
    assert_eq!(r.status, 200);
    assert!(r.headers.iter().any(|(k, v)| k == "content-type" && v == "application/geo+json"));
    let g = r.body_json();
    assert_eq!(g["type"], "FeatureCollection");
    assert_eq!(g["properties"]["includes_reverted"], false);
    // The capture is a delivery inside Chicago.
    let b: Vec<f64> = g["bbox"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(b[0] > -88.5 && b[2] < -87.0 && b[1] > 41.0 && b[3] < 42.5, "{b:?}");
    let features = g["features"].as_array().unwrap();
    let vertices: usize = features.iter().map(|f| f["geometry"]["coordinates"].as_array().unwrap().len()).sum();
    assert_eq!(g["properties"]["vertex_count"], vertices);
    assert!(vertices > 10 && vertices <= 5000);
    for (i, f) in features.iter().enumerate() {
        assert_eq!(f["properties"]["index"], i);
        assert!(f["properties"].get("status").is_none(), "statuses are admin-only");
    }
    // The counted features add up to the job's own game distance.
    let counted: f64 = features.iter().filter(|f| f["properties"]["counted"] == true).map(|f| f["properties"]["game_km"].as_f64().unwrap()).sum();
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}"))).body_json();
    let tracked = detail["distances"]["tracked_counted_game_km"].as_f64().unwrap();
    assert!((counted - tracked).abs() < 0.05 * features.len() as f64, "{counted} vs {tracked}");
    assert!(features.iter().any(|f| f["properties"]["scale_class"] == "city"));

    let a = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job_id}/route"))).body_json();
    assert_eq!(a["properties"]["includes_reverted"], true);
    assert!(a["features"].as_array().unwrap().iter().all(|f| f["properties"]["status"].is_string()));
}

#[test]
fn revocations_reach_the_players_on_that_release() {
    let t = TestHost::new();
    chicago(&t);
    let r = t.call(t.admin("POST", "/api/v1/admin/revocations").with_json(&json!({"target": {"kind": "release", "id": "rel_0_1_0_windows"}, "reason": "Plugin bug"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    assert_eq!(r.body_json()["recipients_count"], 1);
    let ev = t.q("SELECT body FROM webhook_outbox WHERE type = 'version.revocation_scheduled'");
    let b: Value = serde_json::from_str(ev[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(b["data"]["recipients"], json!([PLAYER]));
    assert!(b["data"]["message"].as_str().unwrap().contains("stops counting jobs on 4 November 2026"), "{}", b["data"]["message"]);
    let id = r.body_json()["revocation_id"].as_str().unwrap().to_string();
    t.advance(1);
    let c = t.call(t.admin("POST", &format!("/api/v1/admin/revocations/{id}/cancel")).with_json(&json!({"reason": "Not needed"})));
    assert_eq!(c.body_json()["status"], "cancelled");
    t.advance(1);
    let c = t.call(t.admin("POST", &format!("/api/v1/admin/revocations/{id}/cancel")).with_json(&json!({"reason": "Again"})));
    assert_eq!(c.body_json()["error"]["code"], "invalid_transition");
}

#[test]
fn admins_can_be_added_but_never_all_removed() {
    let t = TestHost::new();
    t.call(t.admin("GET", "/api/v1/admin/admins"));
    let r = t.call(t.admin("DELETE", &format!("/api/v1/admin/admins/{ADMIN}")).with_json(&json!({"reason": "Leaving"})));
    assert_eq!(r.body_json()["error"]["code"], "invalid_transition");
    let r = t.call(t.admin("POST", "/api/v1/admin/admins").with_json(&json!({"discord_user_id": "777", "reason": "New moderator"})));
    assert_eq!(r.status, 201);
    let r = t.call(t.bot("GET", "/api/v1/admin/review-queue/count").with_header("x-plasmatrack-actor", "777"));
    assert_eq!(r.status, 200);
    assert_eq!(r.body_json()["pending_review"], 0);
}
