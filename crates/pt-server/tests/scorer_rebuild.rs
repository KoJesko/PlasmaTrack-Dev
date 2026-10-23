//! A deploy must not re-score stored jobs or tell players again (2026-10-23:
//! the same "Speed penalty" DM for job 3H17-V9C2 after the deploys of 10-15,
//! 10-20 and 10-23, the last one at 155 mph instead of 105). A new scorer
//! format or timeline config rebuilds the state from the raw data, but a
//! stored delivery keeps its outcome and points.

mod common;

use common::seed::*;
use common::TestHost;
use pt_core::Game;
use serde_json::{json, Value};

/// A delivered ATS job at 110 mph for 60 s: over the penalty limit (105 mph),
/// so the player was told once.
fn speeding_job(t: &TestHost) -> Vec<Value> {
    publish(t);
    let token = link(t);
    upload(t, &token, &synthetic_job("0123456789abcdef0123456789abcdef", 110.0, 60.0), 200);
    score(t, Game::Ats);
    let jobs = stored(t);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["state"], "delivered");
    assert!(notifications(t) > 0, "the penalty was announced once");
    jobs
}

fn stored(t: &TestHost) -> Vec<Value> {
    t.q("SELECT job_id, state, outcome, auto_outcome, points, held_points, version, summary, updated_at FROM jobs ORDER BY job_id")
}

/// Webhooks of these types sent so far.
fn sent(t: &TestHost, types: &str) -> usize {
    t.q(&format!("SELECT event_id FROM webhook_outbox WHERE type IN ({types})")).len()
}

/// DMs to the player, and announcements of a job's score.
fn notifications(t: &TestHost) -> usize {
    sent(t, "'player.notification', 'job.scored'")
}

/// The state as a scorer of another format left it.
fn older_format(t: &TestHost) {
    let dbs = t.scorer_dbs.borrow();
    let db = dbs.get(&format!("{}:ats", user_id(t))).expect("the scorer has a database");
    let n = db.0.execute("UPDATE pt_kv SET v = CAST(json_set(CAST(v AS TEXT), '$.key', '3:old') AS BLOB) WHERE k = 'state'", []).unwrap();
    assert_eq!(n, 1);
}

/// Scores until caught up, with a scorer that has nothing in memory (a
/// restarted Durable Object, as after a deploy).
fn rescore(t: &TestHost) -> pt_server::scorer::Report {
    let mut s = t.scorer(&user_id(t), Game::Ats);
    let mut last = common::block_on(s.process(t)).unwrap();
    let rebuilt = last.rebuilt;
    while last.more {
        last = common::block_on(s.process(t)).unwrap();
    }
    last.rebuilt |= rebuilt;
    last
}

#[test]
fn a_new_scorer_format_keeps_stored_jobs_and_tells_nobody_again() {
    let t = TestHost::new();
    let before = speeding_job(&t);
    let told = notifications(&t);

    older_format(&t);
    let r = rescore(&t);
    assert!(r.rebuilt, "{r:?}");
    assert_eq!(stored(&t), before);
    assert_eq!(notifications(&t), told);

    // Caught up: the next pass finds nothing to store.
    let r = rescore(&t);
    assert!(!r.rebuilt);
    assert_eq!(r.jobs_stored, 0, "{r:?}");
    assert_eq!(stored(&t), before);
}

#[test]
fn a_new_speed_limit_does_not_re_score_past_jobs() {
    let t = TestHost::new();
    let before = speeding_job(&t);
    let told = notifications(&t);

    // The timeline config changes (part of the state key): 105 -> 120 mph.
    let toml = pt_server::config::SAMPLE_TOML.replace("penalty_over_mph = 105.0   # rule 5", "penalty_over_mph = 120.0   # rule 5");
    assert_ne!(toml, pt_server::config::SAMPLE_TOML);
    t.advance(1);
    let r = t.call(t.admin("POST", "/api/v1/admin/config-versions").with_json(&json!({"toml": toml, "make_current": true, "reason": "Higher limit"})));
    assert_eq!(r.status, 201, "{}", r.body_json());

    let r = rescore(&t);
    assert!(r.rebuilt, "{r:?}");
    assert_eq!(stored(&t), before, "the penalty stays");
    assert_eq!(notifications(&t), told);

    // A moderator's recalculation applies the new limit on purpose.
    let id = before[0]["job_id"].as_str().unwrap();
    let path = format!("/api/v1/admin/jobs/{id}");
    assert_eq!(t.call(t.admin("GET", &path)).body_json()["sheet"]["speed_penalty"], true);
    common::block_on(pt_server::scorer::rescore_job(&t, id, pt_server::jobs::Trigger::Recalculation, None)).unwrap();
    assert_eq!(t.call(t.admin("GET", &path)).body_json()["sheet"]["speed_penalty"], false);
}

#[test]
fn a_rebuild_still_stores_a_job_not_stored_yet() {
    let t = TestHost::new();
    let before = speeding_job(&t);
    let told = sent(&t, "'player.notification'");
    // As a kind of job tracked only from now on: nothing stored for it.
    t.exec("DELETE FROM jobs");

    older_format(&t);
    let r = rescore(&t);
    assert!(r.rebuilt);
    let now = stored(&t);
    assert_eq!(now.len(), 1);
    assert_eq!(now[0]["state"], "delivered");
    assert_eq!(now[0]["outcome"], before[0]["outcome"]);
    // Announced as a new job, but the player is not told again.
    assert_eq!(sent(&t, "'player.notification'"), told);
}
