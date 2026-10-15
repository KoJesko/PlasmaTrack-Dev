//! Data for the bot's graphs (owner, 2026-10-15): fuel, damage and income on
//! every job, a player's history in buckets, leaderboards by metric, and the
//! job series with distances and damage, public for counted jobs.

mod common;

use common::seed::*;
use common::TestHost;
use serde_json::{json, Value};

fn get(t: &TestHost, path: &str) -> pt_server::http::Response {
    t.call(t.bot("GET", path))
}

#[test]
fn jobs_carry_fuel_damage_and_income() {
    let t = TestHost::new();
    let job = chicago(&t);
    let j = get(&t, &format!("/api/v1/public/jobs/{job}")).body_json();
    assert!(j["fuel_used_l"].as_f64().unwrap() > 0.0, "{j}");
    assert!(j["damage_pct"].is_number(), "{j}");
    assert!(j["income"].as_f64().unwrap() > 0.0, "{j}");
    assert_eq!(j["currency"], "USD");
    let list = get(&t, &format!("/api/v1/public/ats/players/{PLAYER}/jobs")).body_json();
    assert_eq!(list["items"][0]["income"], j["income"]);

    // A moderator's correction shows in the summary.
    let r = t.call(t.admin("PATCH", &format!("/api/v1/admin/jobs/{job}")).with_json(&json!({"reason": "Insurance claim", "damage": 12.5, "profit": 1000})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    let j = get(&t, &format!("/api/v1/public/jobs/{job}")).body_json();
    assert_eq!((j["damage_pct"].as_f64(), j["income"].as_f64()), (Some(12.5), Some(1000.0)));
}

#[test]
fn history_comes_in_buckets_with_running_totals() {
    let t = TestHost::new();
    let job = chicago(&t);
    let j = get(&t, &format!("/api/v1/public/jobs/{job}")).body_json();
    let h = get(&t, &format!("/api/v1/public/ats/players/{PLAYER}/history")).body_json();
    assert_eq!((h["bucket"].as_str(), h["currency"].as_str()), (Some("day"), Some("USD")));
    let items = h["items"].as_array().unwrap();
    assert_eq!(items.len(), 30, "the last 30 days by default");
    let total: i64 = items.iter().map(|i| i["job_count"].as_i64().unwrap()).sum();
    assert_eq!(total, 1);
    let last = items.last().unwrap();
    assert_eq!(last["running"]["points"], 3.6);
    assert_eq!(last["running"]["income"], j["income"]);
    assert_eq!(h["before"]["job_count"], 0);

    // Weeks and months; a window after the job counts it in "before".
    let w = get(&t, &format!("/api/v1/public/ats/players/{PLAYER}/history?bucket=week&from=2026-10-12&to=2026-10-25")).body_json();
    assert_eq!(w["items"].as_array().unwrap().len(), 2);
    assert_eq!(w["items"][0]["start"], "2026-10-12T00:00:00.000Z");
    let m = get(&t, &format!("/api/v1/public/ats/players/{PLAYER}/history?bucket=month&from=2026-11-01&to=2026-12-31")).body_json();
    assert_eq!(m["items"].as_array().unwrap().len(), 2);
    assert_eq!(m["before"]["job_count"], 1);
    assert_eq!(m["items"][1]["running"]["job_count"], 1);
    for bad in ["bucket=hour", "from=yesterday", "from=2020-01-01&to=2026-01-01"] {
        assert_eq!(get(&t, &format!("/api/v1/public/ats/players/{PLAYER}/history?{bad}")).status, 400, "{bad}");
    }
}

#[test]
fn leaderboards_rank_by_any_metric() {
    let t = TestHost::new();
    chicago(&t);
    let other = link_other(&t, "555", "Short Hauler");
    upload(&t, &other, &synthetic_job("0123456789abcdef0123456789abcdef", 50.0, 400.0), 5000);
    let id = t.q("SELECT user_id FROM users WHERE discord_user_id = '555'")[0]["user_id"].as_str().unwrap().to_string();
    common::block_on(pt_server::scorer::Scorer::new(&id, pt_core::Game::Ats).process(&t)).unwrap();
    let board = |q: &str| {
        let v = get(&t, &format!("/api/v1/public/ats/leaderboard?{q}")).body_json();
        let names: Vec<String> = v["items"].as_array().unwrap().iter().map(|i| i["display_name"].as_str().unwrap().to_string()).collect();
        (v, names)
    };
    let (v, _) = board("");
    assert_eq!(v["sort"], "points");
    let pts: Vec<f64> = v["items"].as_array().unwrap().iter().map(|i| i["points"].as_f64().unwrap()).collect();
    assert!(pts.len() == 2 && pts[0] >= pts[1], "{pts:?}");
    let (_, by_fuel_asc) = board("sort=fuel&order=asc");
    let (_, by_fuel) = board("sort=fuel");
    assert_eq!(by_fuel_asc.iter().rev().collect::<Vec<_>>(), by_fuel.iter().collect::<Vec<_>>());
    // Damage ranks the least first; a player without damage data comes last.
    let (v, by_damage) = board("sort=damage");
    assert_eq!(v["order"], "asc");
    assert_eq!(by_damage.last().unwrap(), "Short Hauler", "{}", v);
    assert!(v["items"][0]["damage_pct_avg"].is_number());
    assert!(v["items"][0]["fuel_used_l"].is_number() && v["items"][0]["income"].is_number());
    assert_eq!(get(&t, "/api/v1/public/ats/leaderboard?sort=speed").status, 400);
}

#[test]
fn series_carry_distance_and_damage_and_counted_jobs_are_public() {
    let t = TestHost::new();
    let job = chicago(&t);
    let s = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job}/series?max_points=100"))).body_json();
    let pts = s["points"].as_array().unwrap();
    let game: Vec<f64> = pts.iter().map(|p| p["game_km"].as_f64().unwrap()).collect();
    assert!(game.windows(2).all(|w| w[0] <= w[1]), "cumulative");
    let detail = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job}"))).body_json();
    let tracked = detail["distances"]["tracked_whole_game_km"].as_f64().unwrap();
    assert!((game.last().unwrap() - tracked).abs() < 0.05, "{} vs {tracked}", game.last().unwrap());
    assert!(pts.last().unwrap()["world_km"].as_f64().unwrap() > 0.0);
    assert!(pts.iter().any(|p| p["damage_pct_max"].is_number()));

    // Public for a valid job, without review markers or reverted material.
    let p = get(&t, &format!("/api/v1/public/jobs/{job}/series?max_points=100"));
    assert_eq!(p.status, 200);
    let p = p.body_json();
    assert_eq!(p["markers"], json!([]));
    assert!(p["windows"].as_array().unwrap().iter().all(|w| w["kind"] != "reverted"));
    assert_eq!(p["points"], s["points"]);
    // Not while it is pending or after a rejection.
    t.call(t.admin("POST", &format!("/api/v1/admin/jobs/{job}/reject")).with_json(&json!({"reason": "Testing visibility"})));
    assert_eq!(get(&t, &format!("/api/v1/public/jobs/{job}/series")).status, 404);
    let _: Value = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{job}/series"))).body_json();
}
