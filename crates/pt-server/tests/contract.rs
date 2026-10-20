//! Generates the bot's machine-readable contract from real responses of this
//! server (API_CONVENTIONS §7):
//!
//! - `docs/examples/<name>.json`: one response per endpoint and one body per
//!   webhook type, from a scripted scenario on real captures;
//! - `docs/openapi/bot-api.yaml`: OpenAPI 3.1, every path with its auth,
//!   parameters and example, and response schemas inferred from the examples.
//!
//! The scenario is deterministic (fixed clock, seeded ids), so the files only
//! change when the server's output does. The test fails if they are stale;
//! regenerate with `UPDATE_CONTRACT=1 cargo test -p pt-server --test contract`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::seed::*;
use common::{block_on, TestHost, ADMIN, READ_TOKEN};
use pt_core::Game;
use pt_server::http::Request;
use pt_server::manage;
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, PartialEq)]
enum Auth {
    Read,
    Bot,
    Admin,
    Player,
}

struct Call {
    name: &'static str,
    tag: &'static str,
    summary: &'static str,
    method: &'static str,
    template: &'static str,
    path: String,
    body: Option<Value>,
    auth: Auth,
    status: u16,
    /// An image upload (raw bytes) instead of a JSON body.
    image: bool,
}

struct Recorder<'a> {
    t: &'a TestHost,
    calls: Vec<(Call, Value)>,
    n: usize,
}

impl Recorder<'_> {
    #[allow(clippy::too_many_arguments)]
    fn call(&mut self, name: &'static str, tag: &'static str, summary: &'static str, method: &'static str, template: &'static str, path: &str, body: Option<Value>, auth: Auth, status: u16) -> Value {
        self.n += 1;
        let mut req = match auth {
            Auth::Read => Request::new(method, path).with_header("authorization", &format!("Bearer {READ_TOKEN}")),
            Auth::Bot => self.t.bot(method, path),
            Auth::Admin => self.t.bot(method, path).with_header("x-plasmatrack-actor", ADMIN),
            Auth::Player => Request::new(method, path),
        };
        if method != "GET" {
            req = req.with_header("idempotency-key", &format!("example-{}", self.n));
        }
        if let Some(b) = &body {
            req = req.with_json(b);
        }
        let r = self.t.call(req);
        assert_eq!(r.status, status, "{name}: {}", r.body_json());
        let v = r.body_json();
        let call = Call { name, tag, summary, method, template, path: path.to_string(), body, auth, status, image: false };
        self.calls.push((call, v.clone()));
        self.t.advance(1000);
        v
    }

    /// An image upload: the raw bytes of a (tiny) WebP.
    fn upload_image(&mut self, name: &'static str, summary: &'static str, template: &'static str, path: &str, n: u8) -> Value {
        self.n += 1;
        let mut req = self.t.bot("POST", path).with_header("idempotency-key", &format!("example-{}", self.n)).with_header("content-type", "image/webp");
        req.body = webp(n);
        let r = self.t.call(req);
        assert_eq!(r.status, 201, "{name}: {}", r.body_json());
        let v = r.body_json();
        let call = Call { name, tag: "Manual jobs", summary, method: "POST", template, path: path.to_string(), body: None, auth: Auth::Bot, status: 201, image: true };
        self.calls.push((call, v.clone()));
        self.t.advance(1000);
        v
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---- schema inference --------------------------------------------------------------

fn infer(v: &Value) -> Value {
    match v {
        Value::Null => json!({"type": "null"}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!({"type": "integer"}),
        Value::Number(_) => json!({"type": "number"}),
        Value::String(s) if pt_server::util::parse_rfc3339(s).is_some() => json!({"type": "string", "format": "date-time"}),
        Value::String(_) => json!({"type": "string"}),
        Value::Array(items) => {
            let mut s = Value::Null;
            for i in items {
                s = merge(s, infer(i));
            }
            if s.is_null() {
                json!({"type": "array"})
            } else {
                json!({"type": "array", "items": s})
            }
        }
        Value::Object(o) => {
            let props: Map<String, Value> = o.iter().map(|(k, x)| (k.clone(), infer(x))).collect();
            json!({"type": "object", "properties": props})
        }
    }
}

fn types(s: &Value) -> Vec<Value> {
    match &s["type"] {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        t => vec![t.clone()],
    }
}

/// The union of two inferred schemas: types join, object properties merge.
fn merge(a: Value, b: Value) -> Value {
    if a.is_null() {
        return b;
    }
    let mut ts = types(&a);
    for t in types(&b) {
        if !ts.contains(&t) {
            ts.push(t);
        }
    }
    // An integer seen where a number also appears is just a number.
    if ts.contains(&json!("number")) {
        ts.retain(|t| t != "integer");
    }
    let mut out = Map::new();
    out.insert("type".into(), if ts.len() == 1 { ts[0].clone() } else { Value::Array(ts.clone()) });
    if a.get("format") == b.get("format") || b.get("format").is_none() || a.get("format").is_none() {
        if let Some(f) = a.get("format").or(b.get("format")) {
            out.insert("format".into(), f.clone());
        }
    }
    match (a.get("properties"), b.get("properties")) {
        (Some(pa), Some(pb)) => {
            let mut p = pa.as_object().cloned().unwrap_or_default();
            for (k, v) in pb.as_object().into_iter().flatten() {
                let m = merge(p.remove(k).unwrap_or(Value::Null), v.clone());
                p.insert(k.clone(), m);
            }
            out.insert("properties".into(), Value::Object(p));
        }
        (Some(p), None) | (None, Some(p)) => {
            out.insert("properties".into(), p.clone());
        }
        _ => {}
    }
    match (a.get("items"), b.get("items")) {
        (Some(x), Some(y)) => {
            out.insert("items".into(), merge(x.clone(), y.clone()));
        }
        (Some(x), None) | (None, Some(x)) => {
            out.insert("items".into(), x.clone());
        }
        _ => {}
    }
    Value::Object(out)
}

// ---- YAML (block style; scalars as JSON, which is valid YAML) -----------------------

fn yaml(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(o) if !o.is_empty() => {
            for (k, x) in o {
                let key = serde_json::to_string(k).unwrap();
                match x {
                    Value::Object(m) if !m.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        yaml(x, indent + 2, out);
                    }
                    Value::Array(a) if !a.is_empty() => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        yaml(x, indent, out);
                    }
                    _ => out.push_str(&format!("{pad}{key}: {}\n", serde_json::to_string(x).unwrap())),
                }
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for x in a {
                match x {
                    Value::Object(m) if !m.is_empty() => {
                        let mut inner = String::new();
                        yaml(x, indent + 2, &mut inner);
                        out.push_str(&format!("{pad}- {}", &inner[indent + 2..]));
                    }
                    _ => out.push_str(&format!("{pad}- {}\n", serde_json::to_string(x).unwrap())),
                }
            }
        }
        _ => out.push_str(&format!("{pad}{}\n", serde_json::to_string(v).unwrap())),
    }
}

// ---- the scenario -------------------------------------------------------------------

fn scenario(t: &TestHost) -> (Vec<(Call, Value)>, BTreeMap<String, Value>) {
    use Auth::*;
    publish(t);
    let mut r = Recorder { t, calls: Vec::new(), n: 0 };
    let p = PLAYER;
    r.call("player_token_issue", "Players", "Link a Discord user, or rotate their token", "POST", "/api/v1/players/{discord_id}/token", &format!("/api/v1/players/{p}/token"), Some(json!({"display_name": "Example Driver"})), Bot, 201);
    let token = r.calls.last().unwrap().1["token"]["token"].as_str().unwrap().to_string();
    // The plaintext never goes into the docs.
    r.calls.last_mut().unwrap().1["token"]["token"] = json!("pt_live_EXAMPLE-ONLY-NEVER-A-REAL-TOKEN-000000000");
    for f in ["ats-chicago-clean-delivery.jsonl.gz", "ets2-tmp-norway-tunnel-profiles.jsonl.gz"] {
        upload(t, &token, &fixture(f), 2000);
    }
    let uid = t.q(&format!("SELECT user_id FROM users WHERE discord_user_id = '{p}'"))[0]["user_id"].as_str().unwrap().to_string();
    for g in [Game::Ats, Game::Ets2] {
        let mut s = t.scorer(&uid, g);
        block_on(s.process(t)).unwrap();
    }
    // A player whose install matches no release (job.discarded), and one who
    // delivers at 160 mph (Invalid: job.needs_review and player.notification).
    let other = link_other(t, "345678901234567890", "Unreleased Build");
    let mut bad = manifest();
    bad[0].sha256 = "dd".repeat(32);
    upload_with(t, &other, &fixture("ats-chicago-clean-delivery.jsonl.gz"), 2000, bad);
    let fast = link_other(t, "456789012345678901", "Lead Foot");
    upload(t, &fast, &synthetic_job("0123456789abcdef0123456789abcdef", 160.0, 120.0), 5000);
    for d in ["345678901234567890", "456789012345678901"] {
        let id = t.q(&format!("SELECT user_id FROM users WHERE discord_user_id = '{d}'"))[0]["user_id"].as_str().unwrap().to_string();
        block_on(t.scorer(&id, Game::Ats).process(t)).unwrap();
    }
    let job = |sql: &str| t.q(sql)[0]["job_id"].as_str().unwrap().to_string();
    let valid = job("SELECT job_id FROM jobs WHERE game = 'ats' AND outcome = 'valid'");
    let pending = job("SELECT job_id FROM jobs WHERE outcome = 'pending_review'");

    r.call("player_token_status", "Players", "The active token's metadata", "GET", "/api/v1/players/{discord_id}/token", &format!("/api/v1/players/{p}/token"), None, Bot, 200);
    r.call("player_rename", "Players", "Change the display name", "PATCH", "/api/v1/players/{discord_id}", &format!("/api/v1/players/{p}"), Some(json!({"display_name": "Example Driver"})), Bot, 200);
    r.call("public_player", "Public", "A player's profile, TruckersMP accounts and stats", "GET", "/api/v1/public/players/{discord_id}", &format!("/api/v1/public/players/{p}"), None, Read, 200);
    r.call("public_player_unlinked", "Public", "", "GET", "/api/v1/public/players/{discord_id}", "/api/v1/public/players/42", None, Read, 200);
    r.call("public_player_stats", "Public", "A player's stats in one game", "GET", "/api/v1/public/{game}/players/{discord_id}/stats", &format!("/api/v1/public/ets2/players/{p}/stats"), None, Read, 200);
    r.call("public_player_jobs", "Public", "A player's finished jobs, newest first", "GET", "/api/v1/public/{game}/players/{discord_id}/jobs", &format!("/api/v1/public/ets2/players/{p}/jobs?limit=2"), None, Read, 200);
    r.call("public_job", "Public", "Job detail", "GET", "/api/v1/public/jobs/{job_id}", &format!("/api/v1/public/jobs/{valid}"), None, Read, 200);
    r.call("public_job_pending", "Public", "", "GET", "/api/v1/public/jobs/{job_id}", &format!("/api/v1/public/jobs/{pending}"), None, Read, 200);
    r.call("public_job_route", "Public", "Route GeoJSON", "GET", "/api/v1/public/jobs/{job_id}/route", &format!("/api/v1/public/jobs/{valid}/route"), None, Read, 200);
    r.call("public_player_history", "Public", "A player's totals per day, week or month, with running totals", "GET", "/api/v1/public/{game}/players/{discord_id}/history", &format!("/api/v1/public/ets2/players/{p}/history?bucket=week&from=2026-10-05&to=2026-10-18"), None, Read, 200);
    r.call("public_job_series", "Public", "Speed, fuel, damage and distance over a counted job", "GET", "/api/v1/public/jobs/{job_id}/series", &format!("/api/v1/public/jobs/{valid}/series?max_points=20"), None, Read, 200);
    r.call("public_leaderboard_by_fuel", "Public", "", "GET", "/api/v1/public/{game}/leaderboard", "/api/v1/public/ets2/leaderboard?sort=fuel", None, Read, 200);
    r.call("public_leaderboard", "Public", "Leaderboard", "GET", "/api/v1/public/{game}/leaderboard", "/api/v1/public/ets2/leaderboard?period=month&at=2026-10", None, Read, 200);

    r.call("admin_review_queue", "Review", "Jobs waiting for a moderator", "GET", "/api/v1/admin/review-queue", "/api/v1/admin/review-queue", None, Admin, 200);
    r.call("admin_review_queue_count", "Review", "Review queue counts", "GET", "/api/v1/admin/review-queue/count", "/api/v1/admin/review-queue/count", None, Admin, 200);
    let d = r.call("admin_job", "Jobs", "Admin job detail", "GET", "/api/v1/admin/jobs/{job_id}", &format!("/api/v1/admin/jobs/{pending}"), None, Admin, 200);
    r.call("admin_job_series", "Jobs", "Speed and fuel series", "GET", "/api/v1/admin/jobs/{job_id}/series", &format!("/api/v1/admin/jobs/{valid}/series?max_points=50"), None, Admin, 200);
    r.call("admin_job_route", "Jobs", "Admin route GeoJSON with reverted material", "GET", "/api/v1/admin/jobs/{job_id}/route", &format!("/api/v1/admin/jobs/{valid}/route"), None, Admin, 200);
    r.call("admin_job_trace", "Jobs", "A signed download link for the raw trace", "GET", "/api/v1/admin/jobs/{job_id}/trace", &format!("/api/v1/admin/jobs/{valid}/trace"), None, Admin, 200);
    r.call("admin_job_approve", "Decisions", "Approve, reject, void or restore a job", "POST", "/api/v1/admin/jobs/{job_id}/{action}", &format!("/api/v1/admin/jobs/{pending}/approve"), Some(json!({"reason": "Checked the route: fine.", "expected_etag": d["etag"]})), Admin, 200);
    r.call("error_invalid_transition", "Decisions", "", "POST", "/api/v1/admin/jobs/{job_id}/{action}", &format!("/api/v1/admin/jobs/{pending}/restore"), Some(json!({"reason": "Not voided."})), Admin, 409);
    r.call("error_validation", "Decisions", "", "POST", "/api/v1/admin/jobs/{job_id}/{action}", &format!("/api/v1/admin/jobs/{pending}/reject"), Some(json!({})), Admin, 422);
    r.call("admin_job_recalculate", "Recalculation", "Re-score one job now", "POST", "/api/v1/admin/jobs/{job_id}/recalculate", &format!("/api/v1/admin/jobs/{valid}/recalculate"), Some(json!({"reason": "Checking a rule change."})), Admin, 200);

    // Manual submissions (PUBLIC_API §6) and moderator edits (ADMIN_API §14).
    let ev = r.upload_image("player_evidence_upload", "Upload one evidence image", "/api/v1/players/{discord_id}/evidence", &format!("/api/v1/players/{p}/evidence"), 1);
    let (map, uk, adj) = (image(t, p, 2), image(t, p, 3), image(t, p, 4));
    let mut submitted = manual_body(ev["evidence_id"].as_str().unwrap(), &map, &uk);
    submitted["adjusted_scales"] = json!([{"entry_city": "Paris", "exit_city": "Paris", "distance": 6, "scale": 3, "evidence": adj}]);
    let mut bad = manual_body(ev["evidence_id"].as_str().unwrap(), &map, &uk);
    bad["damage"] = json!(140);
    r.call("error_validation_manual", "Manual jobs", "", "POST", "/api/v1/players/{discord_id}/manual-jobs", &format!("/api/v1/players/{p}/manual-jobs"), Some(bad), Bot, 422);
    let m = r.call("player_manual_job_submit", "Manual jobs", "Submit a job by hand (goes to review)", "POST", "/api/v1/players/{discord_id}/manual-jobs", &format!("/api/v1/players/{p}/manual-jobs"), Some(submitted), Bot, 201);
    let manual = m["job_id"].as_str().unwrap().to_string();
    r.call("player_job_evidence", "Manual jobs", "The submitter's own evidence images", "GET", "/api/v1/players/{discord_id}/jobs/{job_id}/evidence", &format!("/api/v1/players/{p}/jobs/{manual}/evidence"), None, Bot, 200);
    r.call("public_job_manual", "Public", "", "GET", "/api/v1/public/jobs/{job_id}", &format!("/api/v1/public/jobs/{manual}"), None, Read, 200);
    let md = r.call("admin_job_manual", "Jobs", "", "GET", "/api/v1/admin/jobs/{job_id}", &format!("/api/v1/admin/jobs/{manual}"), None, Admin, 200);
    r.call("admin_job_approve_manual", "Decisions", "", "POST", "/api/v1/admin/jobs/{job_id}/{action}", &format!("/api/v1/admin/jobs/{manual}/approve"), Some(json!({"reason": "Screenshots match.", "expected_etag": md["etag"], "hide_map": false, "corrections": {"unit": "km", "distance": 384, "uk": {"distance": 80}, "adjusted_scales": {"0": {"distance": 7}}}})), Admin, 200);
    r.call("admin_job_edit", "Jobs", "Correct any field of any job", "PATCH", "/api/v1/admin/jobs/{job_id}", &format!("/api/v1/admin/jobs/{valid}"), Some(json!({"reason": "The cargo was renamed by a mod.", "cargo_name": "Corrected cargo", "damage": 3})), Admin, 200);
    r.call("public_player_jobs_all", "Public", "A player's jobs in both games, newest first", "GET", "/api/v1/public/players/{discord_id}/jobs", &format!("/api/v1/public/players/{p}/jobs?limit=3"), None, Read, 200);

    let b = r.call("admin_bonus_create", "Bonuses", "Create a bonus", "POST", "/api/v1/admin/bonuses", "/api/v1/admin/bonuses", Some(json!({"game": "ets2", "label": "Medical week", "match": "med_*", "match_kind": "glob", "kind": "multiplier", "amount": 1.5, "active_from": "2026-10-01T00:00:00.000Z", "active_to": "2026-11-01T00:00:00.000Z", "reason": "October event"})), Admin, 201);
    let bid = b["bonus_id"].as_str().unwrap().to_string();
    r.call("admin_bonuses", "Bonuses", "List bonuses", "GET", "/api/v1/admin/bonuses", "/api/v1/admin/bonuses?game=ets2", None, Admin, 200);
    r.call("admin_bonus", "Bonuses", "One bonus", "GET", "/api/v1/admin/bonuses/{bonus_id}", &format!("/api/v1/admin/bonuses/{bid}"), None, Admin, 200);
    r.call("admin_bonus_update", "Bonuses", "Change a bonus", "PATCH", "/api/v1/admin/bonuses/{bonus_id}", &format!("/api/v1/admin/bonuses/{bid}"), Some(json!({"amount": 2.0, "reason": "Doubled"})), Admin, 200);
    r.call("admin_recalculations_dry_run", "Recalculation", "Re-score every job (asynchronous); dry run counts", "POST", "/api/v1/admin/recalculations", "/api/v1/admin/recalculations", Some(json!({"reason": "Bonus added", "game": "ets2", "dry_run": true})), Admin, 200);
    let op = r.call("admin_recalculations", "Recalculation", "", "POST", "/api/v1/admin/recalculations", "/api/v1/admin/recalculations", Some(json!({"reason": "Bonus added", "game": "ets2"})), Admin, 202);
    let msg = t.ops_queued.borrow().last().cloned().unwrap();
    block_on(manage::run_op(t, &msg)).unwrap();
    r.call("admin_operation", "Recalculation", "A bulk operation's progress", "GET", "/api/v1/admin/operations/{operation_id}", &format!("/api/v1/admin/operations/{}", op["operation_id"].as_str().unwrap()), None, Admin, 200);
    r.call("admin_bonus_archive", "Bonuses", "Archive a bonus", "DELETE", "/api/v1/admin/bonuses/{bonus_id}", &format!("/api/v1/admin/bonuses/{bid}"), Some(json!({"reason": "Event over"})), Admin, 200);

    r.call("admin_player", "Players (admin)", "A player with role, token metadata and flags", "GET", "/api/v1/admin/players/{discord_id}", &format!("/api/v1/admin/players/{p}"), None, Admin, 200);
    r.call("admin_player_update", "Players (admin)", "Singleplayer Division, disable, rename", "PATCH", "/api/v1/admin/players/{discord_id}", &format!("/api/v1/admin/players/{p}"), Some(json!({"singleplayer_division": true, "reason": "Asked to join the division"})), Admin, 200);
    r.call("admin_player_token_issue", "Players (admin)", "Issue a token for a player", "POST", "/api/v1/admin/players/{discord_id}/token", "/api/v1/admin/players/234567890123456789/token", Some(json!({"reason": "New player", "display_name": "Second Driver"})), Admin, 201);
    r.calls.last_mut().unwrap().1["token"]["token"] = json!("pt_live_EXAMPLE-ONLY-NEVER-A-REAL-TOKEN-000000000");
    r.call("admin_player_token_revoke", "Players (admin)", "Revoke a player's token", "DELETE", "/api/v1/admin/players/{discord_id}/token", "/api/v1/admin/players/234567890123456789/token", Some(json!({"reason": "Left the community"})), Admin, 200);
    r.call("player_token_revoke", "Players", "Revoke one's own token", "DELETE", "/api/v1/players/{discord_id}/token", &format!("/api/v1/players/{p}/token"), None, Bot, 200);

    r.call("admin_config_versions", "Versions", "Published config versions", "GET", "/api/v1/admin/config-versions", "/api/v1/admin/config-versions", None, Admin, 200);
    let toml = pt_server::config::SAMPLE_TOML.replace("speed_penalty_factor = 0.85", "speed_penalty_factor = 0.8");
    r.call("admin_config_version_create", "Versions", "Publish a config version", "POST", "/api/v1/admin/config-versions", "/api/v1/admin/config-versions", Some(json!({"toml": toml, "make_current": false, "reason": "Trying a gentler penalty"})), Admin, 201);
    r.call("admin_config_version_make_current", "Versions", "Make a config version current", "POST", "/api/v1/admin/config-versions/{version_id}/make-current", "/api/v1/admin/config-versions/cfg_1/make-current", Some(json!({"reason": "Back to the first version"})), Admin, 200);
    r.call("admin_releases", "Versions", "Published releases", "GET", "/api/v1/admin/releases", "/api/v1/admin/releases", None, Admin, 200);
    r.call("admin_release_create", "Versions", "Publish a release's file hashes", "POST", "/api/v1/admin/releases", "/api/v1/admin/releases", Some(json!({"release_version": "0.2.0", "platform": "windows", "files": [{"role": "plugin", "path_pattern": "*/plugins/plasmatrack.dll", "sha256": "cc".repeat(32)}], "reason": "Release 0.2.0"})), Admin, 201);
    let up = common::signed_update(t, "0.3.0");
    let files: Vec<Value> = up.files.clone();
    r.call("admin_release_create_update", "Versions", "", "POST", "/api/v1/admin/releases", "/api/v1/admin/releases", Some(json!({"release_version": "0.3.0", "platform": "windows", "files": files, "update": {"manifest": up.manifest, "signature": up.signature}, "installer": "PlasmaTrack-Setup-0.3.0.exe", "notes": "Uploads resume faster after a network outage.", "offer": false, "reason": "Release 0.3.0"})), Admin, 201);
    r.call("admin_release_offer", "Versions", "Start or stop offering a release's update to clients", "POST", "/api/v1/admin/releases/{release_id}/offer", "/api/v1/admin/releases/rel_0_3_0_windows/offer", Some(json!({"offered": true, "reason": "Tested on the test server"})), Admin, 200);
    r.call("public_client", "Public", "The newest PlasmaTrack release and its setup download link", "GET", "/api/v1/public/client", "/api/v1/public/client?platform=windows", None, Read, 200);
    r.call("player_client_download", "Players", "A personal one-hour link to the newest setup program (the bot checks Discord roles first)", "GET", "/api/v1/players/{discord_id}/client-download", &format!("/api/v1/players/{p}/client-download?platform=windows"), None, Bot, 200);
    let rev = r.call("admin_revocation_create", "Versions", "Schedule a revocation (21 days)", "POST", "/api/v1/admin/revocations", "/api/v1/admin/revocations", Some(json!({"target": {"kind": "release", "id": "rel_0_1_0_windows"}, "reason": "Plugin bug fixed in 0.2.0"})), Admin, 201);
    r.call("admin_revocations", "Versions", "Revocations", "GET", "/api/v1/admin/revocations", "/api/v1/admin/revocations?status=scheduled", None, Admin, 200);
    r.call("admin_revocation_cancel", "Versions", "Cancel a scheduled revocation", "POST", "/api/v1/admin/revocations/{revocation_id}/cancel", &format!("/api/v1/admin/revocations/{}/cancel", rev["revocation_id"].as_str().unwrap()), Some(json!({"reason": "Not needed after all"})), Admin, 200);

    r.call("admin_admins", "Admins", "List admins", "GET", "/api/v1/admin/admins", "/api/v1/admin/admins", None, Admin, 200);
    r.call("admin_admin_add", "Admins", "Add an admin", "POST", "/api/v1/admin/admins", "/api/v1/admin/admins", Some(json!({"discord_user_id": "987654321098765432", "reason": "New moderator"})), Admin, 201);
    r.call("admin_admin_remove", "Admins", "Remove an admin", "DELETE", "/api/v1/admin/admins/{discord_id}", "/api/v1/admin/admins/987654321098765432", Some(json!({"reason": "Stepped down"})), Admin, 200);
    r.call("admin_truckersmp_flags", "Review", "TruckersMP account flags", "GET", "/api/v1/admin/truckersmp/flags", "/api/v1/admin/truckersmp/flags", None, Admin, 200);
    r.call("admin_backup_create", "Admins", "Back up the database now (also weekly, automatically)", "POST", "/api/v1/admin/backups", "/api/v1/admin/backups", Some(json!({"reason": "Before a config change"})), Admin, 201);
    r.call("admin_audit", "Audit", "The audit log, newest first", "GET", "/api/v1/admin/audit", &format!("/api/v1/admin/audit?job_id={pending}"), None, Admin, 200);
    r.call("error_unauthorized", "Public", "", "GET", "/api/v1/public/{game}/leaderboard", "/api/v1/public/ets2/leaderboard?cursor=", None, Player, 401);

    // Webhook bodies, one per type the scenario produced.
    let mut hooks = BTreeMap::new();
    for row in t.q("SELECT type, body FROM webhook_outbox ORDER BY created_at, event_id") {
        let ty = row["type"].as_str().unwrap().to_string();
        hooks.entry(ty).or_insert_with(|| serde_json::from_str(row["body"].as_str().unwrap()).unwrap());
    }
    (r.calls, hooks)
}

fn openapi(calls: &[(Call, Value)], hooks: &BTreeMap<String, Value>) -> Value {
    let mut paths: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut schemas: BTreeMap<(String, String, u16), Value> = BTreeMap::new();
    for (c, v) in calls {
        let key = (c.template.to_string(), c.method.to_lowercase(), c.status);
        let s = schemas.remove(&key).unwrap_or(Value::Null);
        schemas.insert(key, merge(s, infer(v)));
    }
    for (c, _) in calls {
        let method = c.method.to_lowercase();
        let ops = paths.entry(c.template.to_string()).or_default();
        let op = ops.entry(method.clone()).or_insert_with(|| {
            let security = match c.auth {
                Auth::Read => json!([{"readToken": []}, {"botToken": []}]),
                Auth::Bot => json!([{"botToken": []}]),
                Auth::Admin => json!([{"botToken": []}]),
                Auth::Player => json!([{"readToken": []}, {"botToken": []}]),
            };
            let mut params: Vec<Value> = c
                .template
                .split('/')
                .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
                .map(|name| {
                    let schema = match name {
                        "game" => json!({"type": "string", "enum": ["ets2", "ats"]}),
                        "action" => json!({"type": "string", "enum": ["approve", "reject", "void", "restore"]}),
                        _ => json!({"type": "string"}),
                    };
                    json!({"name": name, "in": "path", "required": true, "schema": schema})
                })
                .collect();
            if c.auth == Auth::Admin {
                params.push(json!({"$ref": "#/components/parameters/Actor"}));
            }
            if c.method != "GET" {
                params.push(json!({"$ref": "#/components/parameters/IdempotencyKey"}));
            }
            json!({"tags": [c.tag], "summary": c.summary, "operationId": c.name, "security": security,
                   "parameters": params, "responses": {
                       "4XX": {"$ref": "#/components/responses/Error"},
                       "5XX": {"$ref": "#/components/responses/Error"}}})
        });
        if op["summary"] == "" && !c.summary.is_empty() {
            op["summary"] = c.summary.into();
        }
        if let Some(q) = c.path.split_once('?').map(|(_, q)| q) {
            for (k, _) in q.split('&').filter_map(|kv| kv.split_once('=')) {
                let params = op["parameters"].as_array_mut().unwrap();
                if !params.iter().any(|p| p["name"] == k) {
                    params.push(json!({"name": k, "in": "query", "required": false, "schema": {"type": "string"}}));
                }
            }
        }
        if c.image && op.get("requestBody").is_none() {
            let bin = json!({"schema": {"type": "string", "format": "binary"}});
            op["requestBody"] = json!({"required": true, "description": "The image itself, at most 5 MB.",
                "content": {"image/avif": bin, "image/webp": bin, "image/png": bin, "image/jpeg": bin}});
        }
        if let (Some(b), None) = (&c.body, op.get("requestBody")) {
            op["requestBody"] = json!({"required": true, "content": {"application/json": {"example": b}}});
        }
        let status = c.status.to_string();
        if op["responses"].get(&status).is_none() {
            let schema = if c.status >= 400 {
                json!({"$ref": "#/components/schemas/Error"})
            } else {
                schemas[&(c.template.to_string(), c.method.to_lowercase(), c.status)].clone()
            };
            let media = if c.template.ends_with("/route") && c.status == 200 { "application/geo+json" } else { "application/json" };
            op["responses"][&status] = json!({"description": format!("See docs/examples/{}.json", c.name),
                "content": {media: {"schema": schema,
                    "examples": {c.name: {"externalValue": format!("../examples/{}.json", c.name)}}}}});
        }
    }
    let mut webhook_doc = Map::new();
    for (ty, body) in hooks {
        webhook_doc.insert(ty.clone(), json!({"post": {
            "summary": format!("{ty} (docs/WEBHOOKS.md §4)"),
            "operationId": format!("webhook_{}", ty.replace('.', "_")),
            "security": [],
            "parameters": [
                {"name": "X-PlasmaTrack-Event", "in": "header", "required": true, "schema": {"type": "string", "const": ty}},
                {"name": "X-PlasmaTrack-Event-Id", "in": "header", "required": true, "schema": {"type": "string"}},
                {"name": "X-PlasmaTrack-Signature", "in": "header", "required": true, "schema": {"type": "string"},
                 "description": "t=<unix seconds>,v1=<hex HMAC-SHA256 of \"{t}.{raw body}\">[,v1=…]"}],
            "requestBody": {"content": {"application/json": {"schema": infer(body),
                "examples": {"example": {"externalValue": format!("../examples/webhook_{}.json", ty.replace('.', "_"))}}}}},
            "responses": {"202": {"description": "Queued by the bot"}}}}));
    }
    json!({
        "openapi": "3.1.0",
        "info": {"title": "PlasmaTrack bot API", "version": "1",
                 "license": {"name": "PolyForm Noncommercial 1.0.0", "identifier": "PolyForm-Noncommercial-1.0.0"},
                 "description": "Generated by crates/pt-server/tests/contract.rs from real responses of the server. The Markdown contract (docs/API_CONVENTIONS.md, PUBLIC_API.md, ADMIN_API.md, WEBHOOKS.md) is authoritative; response schemas here are inferred from the examples, so a field shown as one type may also be null where the contract says so."},
        "servers": [{"url": "https://plasmatrack-server.{account}.workers.dev", "description": "production",
                     "variables": {"account": {"default": "example"}}},
                    {"url": "https://plasmatrack-server-test.{account}.workers.dev", "description": "test",
                     "variables": {"account": {"default": "example"}}}],
        "paths": paths,
        "webhooks": webhook_doc,
        "components": {
            "securitySchemes": {
                "readToken": {"type": "http", "scheme": "bearer", "description": "Public reads only"},
                "botToken": {"type": "http", "scheme": "bearer", "description": "Everything"}},
            "parameters": {
                "Actor": {"name": "X-PlasmaTrack-Actor", "in": "header", "required": true,
                          "description": "Discord ID of the acting admin", "schema": {"type": "string", "pattern": "^[0-9]{1,20}$"}},
                "IdempotencyKey": {"name": "Idempotency-Key", "in": "header", "required": true,
                                   "description": "1-128 printable ASCII; remembered 24 h", "schema": {"type": "string", "minLength": 1, "maxLength": 128}}},
            "responses": {"Error": {"description": "The error shape of API_CONVENTIONS §4",
                "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}},
            "schemas": {"Error": {"type": "object", "required": ["error"], "properties": {"error": {"type": "object",
                "required": ["code", "message", "details"], "properties": {"code": {"type": "string"},
                "message": {"type": "string"}, "details": {"type": "object"}}}}}}
        }
    })
}

#[test]
fn contract_files_match_the_server() {
    let t = TestHost::new();
    let (calls, hooks) = scenario(&t);
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();
    let ex = root().join("docs/examples");
    for (c, v) in &calls {
        files.insert(ex.join(format!("{}.json", c.name)), serde_json::to_string_pretty(v).unwrap() + "\n");
    }
    for (ty, body) in &hooks {
        files.insert(ex.join(format!("webhook_{}.json", ty.replace('.', "_"))), serde_json::to_string_pretty(body).unwrap() + "\n");
    }
    let mut y = String::from("# Generated by crates/pt-server/tests/contract.rs. Do not edit; run\n# UPDATE_CONTRACT=1 cargo test -p pt-server --test contract\n");
    yaml(&openapi(&calls, &hooks), 0, &mut y);
    files.insert(root().join("docs/openapi/bot-api.yaml"), y);

    let update = std::env::var_os("UPDATE_CONTRACT").is_some();
    let mut stale = Vec::new();
    for (path, text) in &files {
        let have = std::fs::read_to_string(path).unwrap_or_default();
        if have.replace("\r\n", "\n") != *text {
            if update {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            } else {
                stale.push(path.display().to_string());
            }
        }
    }
    assert!(stale.is_empty(), "stale contract files (run with UPDATE_CONTRACT=1): {stale:#?}");
}
