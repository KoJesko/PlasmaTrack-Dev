//! Turning a reconstructed job into its stored row, its events and its JSON
//! views (PUBLIC_API §3, ADMIN_API §1-4).

use pt_core::config::ScaleClass;
use pt_core::rules::{evaluate, Bonus, BonusKind, Context, DiscardReason, JobResult, Outcome, ReviewReason};
use pt_core::timeline::{Job, JobEvent};
use pt_core::{Config, Game};
use serde_json::{json, Map, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::ApiResult;
use crate::params;
use crate::store::{self, User};
use crate::util::{self, mi, rfc3339, round2};
use crate::webhooks;

/// Outcomes a moderator set; they survive re-scoring (ADMIN_API §8).
pub const DECIDED: [&str; 3] = ["approved", "rejected", "voided"];

pub fn job_key(user_id: &str, game: Game, profile: &str, local_id: u32) -> String {
    format!("jobs/{user_id}/{}/{profile}/{local_id}", game.as_str())
}

pub fn blob_key(job_id: &str) -> String {
    format!("jobs/{job_id}.json.gz")
}

/// What the scorer knows about a job beyond the job itself.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Server clock when the delivery reached the server.
    pub delivered_at: Option<i64>,
    /// Plugin sessions of the job, and each point's `sess` index → session id.
    pub sessions: Vec<String>,
    pub sess_ids: Vec<(u32, String)>,
    /// TruckersMP server and account while on the job (latest seen).
    pub truckersmp: Option<(u64, u64)>,
}

impl Facts {
    pub fn sess_of(&self, idx: u32) -> &str {
        self.sess_ids.iter().find(|(i, _)| *i == idx).map(|(_, s)| s.as_str()).unwrap_or("")
    }
}

/// Why a job was scored now: decides `by.kind` and whether players are told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// New data from the player's client.
    Upload,
    /// A rebuild from raw frames (out-of-order data, a timeline config change).
    Rebuild,
    /// A moderator's or bulk recalculation.
    Recalculation,
}

// ---- rule 0 ------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Integrity {
    pub ok: bool,
    pub json: Value,
}

/// Rule 0 for a job's sessions (PLAN §6a): each session's config must be a
/// published version and every installed file must belong to a published
/// release; a revocation counts once it took effect before the delivery.
pub async fn integrity<H: Host>(h: &H, user_id: &str, sessions: &[String], at_ms: i64) -> ApiResult<Integrity> {
    let releases = h.all("SELECT * FROM releases", vec![]).await?;
    let revoked: Vec<Row> = h
        .all("SELECT target_kind, target_id FROM revocations WHERE cancelled_at IS NULL AND effective_at <= ?1", params![at_ms])
        .await?;
    let is_revoked = |kind: &str, id: &str| revoked.iter().any(|r| r.s("target_kind") == kind && r.s("target_id") == id);
    let mut ok = true;
    let (mut config_hash, mut config_matched, mut manifest_out, mut manifest_result) =
        (String::new(), Value::Null, Vec::new(), "ok".to_string());
    let mut trace_checks = Vec::new();
    for sess in sessions {
        let Some(s) = store::one(h, "SELECT * FROM plugin_sessions WHERE user_id = ?1 AND sess = ?2", params![user_id, sess]).await? else {
            continue;
        };
        config_hash = s.s("config_hash");
        let version = store::one(h, "SELECT version_id FROM config_versions WHERE integrity_hash = ?1", params![&config_hash]).await?;
        match version.map(|v| v.s("version_id")) {
            Some(v) if !is_revoked("config_version", &v) => config_matched = v.into(),
            Some(v) => {
                ok = false;
                config_matched = json!({"version_id": v, "revoked": true});
            }
            None => {
                ok = false;
                config_matched = Value::Null;
            }
        }
        let entries: Vec<Value> = serde_json::from_str(&s.s("manifest")).unwrap_or_default();
        let mut plugins = 0;
        for e in entries {
            let (role, path, sha) = (e["role"].as_str().unwrap_or(""), e["path"].as_str().unwrap_or(""), e["sha256"].as_str().unwrap_or(""));
            if role == "config_file" {
                // Holds [local] (the token): checked through config_hash instead.
                continue;
            }
            plugins += (role == "plugin") as usize;
            let matched = releases.iter().find(|r| {
                let files: Vec<Value> = serde_json::from_str(&r.s("files")).unwrap_or_default();
                files.iter().any(|f| {
                    f["role"] == role
                        && f["sha256"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(sha))
                        && util::glob(f["path_pattern"].as_str().unwrap_or("*"), path)
                })
            });
            let matched_release = match matched {
                Some(r) if is_revoked("release", &r.s("release_id")) => {
                    ok = false;
                    manifest_result = "revoked".into();
                    Value::String(r.s("release_id"))
                }
                Some(r) => Value::String(r.s("release_id")),
                None => {
                    ok = false;
                    manifest_result = "unmatched".into();
                    Value::Null
                }
            };
            manifest_out.push(json!({"role": role, "path": path, "sha256": sha, "matched_release": matched_release}));
        }
        if plugins == 0 {
            ok = false;
            manifest_result = "missing_plugin".into();
        }
        trace_checks.push(json!({"sess": sess, "hello": s.json("hello")}));
    }
    if sessions.is_empty() {
        ok = false;
        manifest_result = "no_session".into();
    }
    Ok(Integrity {
        ok,
        json: json!({
            "config_hash": config_hash,
            "config_matched_version": config_matched,
            "config_diff": [],
            "manifest_result": manifest_result,
            "manifest": manifest_out,
            "hellos": trace_checks,
        }),
    })
}

/// The sample-rate trace check (PLAN §6a): more samples per simulated second
/// than the plugin was configured for means a modified plugin or client. Fewer
/// are normal at low frame rates.
pub fn trace_checks(job: &Job, cfg: &Config, hellos: &Value) -> (bool, Vec<Value>) {
    let declared = cfg.plugin.sample_rate_hz;
    let (mut n, mut secs) = (0usize, 0.0f64);
    for w in job.kept.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.sess == b.sess && a.run == b.run {
            let dt = b.sim_us.saturating_sub(a.sim_us) as f64 / 1e6;
            if dt > 0.0 && dt < 1.0 {
                n += 1;
                secs += dt;
            }
        }
    }
    let measured = if secs > 0.0 { n as f64 / secs } else { 0.0 };
    let rate_ok = secs < 10.0 || measured <= declared * 1.25 + 0.5;
    let mut checks = vec![json!({"check": "sample_rate", "ok": rate_ok,
        "details": {"declared_hz": declared, "measured_hz": util::round2(measured)}})];
    let mut ok = rate_ok;
    for hl in hellos.as_array().into_iter().flatten() {
        if let Some(hz) = hl["hello"]["sample_rate_hz"].as_f64() {
            let same = (hz - declared).abs() < 0.01;
            ok &= same;
            checks.push(json!({"check": "hello_sample_rate", "ok": same,
                "details": {"sess": hl["sess"], "hello_hz": hz, "config_hz": declared}}));
        }
    }
    (ok, checks)
}

// ---- views --------------------------------------------------------------------------

pub fn class_name(c: ScaleClass) -> &'static str {
    match c {
        ScaleClass::City => "city",
        ScaleClass::Road => "road",
        ScaleClass::OneToOne => "one_to_one",
        ScaleClass::Nonstandard => "nonstandard",
        ScaleClass::Unknown => "unknown",
    }
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Valid => "valid",
        Outcome::PendingReview => "pending_review",
        Outcome::Invalid => "invalid",
        Outcome::Discarded => "discarded",
        Outcome::Incomplete => "incomplete",
    }
}

fn reason_text(code: &str) -> &'static str {
    match code {
        "long_job" => "The loaded 1:1 distance is over the review threshold.",
        "fast_average" => "The whole job averaged over the allowed 1:1 pace per real minute.",
        "fast_average_game" => "The whole job averaged over the allowed pace by the game's clock.",
        "negative_fuel" => "Fuel was used up faster than possible in a run.",
        "fuel_jump" => "Fuel rose outside a detected refuel.",
        "gap" => "Data is missing mid-job.",
        "late_start" => "Tracking started after the job was accepted.",
        "cargo_switch" => "The cargo changed on the same route mid-job.",
        "distance_mismatch" => "The game's delivery distance disagrees with the tracked distance.",
        "no_delivered_distance" => "The delivery carried no distance.",
        "trace_check" => "The trace does not match the configured sample rate.",
        "overspeed_invalid" => "Sustained speed over the invalid limit.",
        "integrity" => "The tracker's files or settings were not recognised.",
        "negative_distance" => "The odometer ran backwards.",
        "singleplayer_division_truckersmp" => "A TruckersMP job by a Singleplayer Division player.",
        _ => "",
    }
}

fn reason(code: &str, details: Value) -> Value {
    json!({"code": code, "text": reason_text(code), "details": details})
}

fn review_details(r: &ReviewReason, job: &Job, cfg: &Config) -> Value {
    match r {
        ReviewReason::LongJob { world_km } => {
            json!({"world_km": round2(*world_km), "threshold_world_km": cfg.rules.manual_review_over_world_km})
        }
        ReviewReason::FastAverage { world_km, driving_min, world_km_per_min } => {
            json!({"world_km": round2(*world_km), "driving_min": round2(*driving_min), "world_km_per_min": round2(*world_km_per_min)})
        }
        ReviewReason::FastAverageGame { world_km, game_clock_min, world_km_per_min } => {
            json!({"world_km": round2(*world_km), "game_clock_min": round2(*game_clock_min), "world_km_per_min": round2(*world_km_per_min)})
        }
        ReviewReason::NegativeFuel { run, used_l } => json!({"run": run, "used_l": round2(*used_l)}),
        ReviewReason::FuelJump { wall_ms, litres } => json!({"at": rfc3339(*wall_ms), "litres": round2(*litres)}),
        ReviewReason::Gap { count } => json!({"count": count}),
        ReviewReason::CargoSwitch => {
            let (from, to) = job
                .events
                .iter()
                .find_map(|e| match e {
                    JobEvent::CargoSwitch { from, to, .. } => Some((from.cargo_id.clone(), to.cargo_id.clone())),
                    _ => None,
                })
                .unwrap_or_default();
            json!({"from_cargo": from, "to_cargo": to})
        }
        ReviewReason::DistanceMismatch { delivered_km, tracked_km, tolerance_km } => {
            json!({"delivered_km": round2(*delivered_km), "tracked_km": round2(*tracked_km), "tolerance_km": round2(*tolerance_km)})
        }
        ReviewReason::LateStart | ReviewReason::NoDeliveredDistance => json!({}),
    }
}

fn event_json(e: &JobEvent) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_default();
    let obj = v.as_object_mut().expect("events serialise as objects");
    let kind = obj.remove("kind").unwrap_or_default();
    let at = obj.remove("wall_ms").and_then(|w| w.as_i64()).map(rfc3339);
    json!({"at": at, "event": kind, "details": Value::Object(obj.clone())})
}

/// Public notes (PUBLIC_API §3) for an outcome.
pub fn public_notes(outcome: &str, summary: &Value, discard: Option<&str>, cfg_factor: f64) -> Vec<Value> {
    let mut notes = Vec::new();
    if summary["speed_penalty_applied"] == true && !matches!(outcome, "discarded" | "voided" | "rejected" | "invalid") {
        notes.push(json!({"code": "speed_penalty", "text": format!("Speed penalty applied: {cfg_factor}× distance.")}));
    }
    match outcome {
        "pending_review" => notes.push(json!({"code": "under_review", "text": "A moderator will check this job."})),
        "invalid" => notes.push(json!({"code": "invalid_overspeed", "text": "Sustained speed over the invalid limit: no points."})),
        "rejected" => notes.push(json!({"code": "rejected", "text": "Rejected by a moderator."})),
        "voided" => notes.push(json!({"code": "voided", "text": "Voided by a moderator."})),
        "discarded" => {
            let (code, text) = match discard {
                Some("negative_distance") => ("discarded_negative_distance", "Discarded: the odometer ran backwards."),
                Some("singleplayer_division_truckersmp") => {
                    ("discarded_singleplayer_division", "Discarded: a TruckersMP job by a Singleplayer Division player.")
                }
                _ => ("discarded_integrity", "Discarded: the tracker's files or settings were not recognised."),
            };
            notes.push(json!({"code": code, "text": text}));
        }
        _ => {}
    }
    notes
}

fn place(city_id: &str, city: &str, company_id: &str, company: &str) -> Value {
    json!({"city_id": city_id, "city_name": city, "company_id": company_id, "company_name": company})
}

/// Fuel used: every drop between neighbouring points of one run.
fn fuel_used(job: &Job) -> f64 {
    job.kept
        .windows(2)
        .filter(|w| w[0].run == w[1].run)
        .filter_map(|w| Some((w[0].fuel_l? - w[1].fuel_l?).max(0.0)))
        .sum()
}

struct Built {
    summary: Value,
    detail: Value,
    reasons: Vec<Value>,
    auto_outcome: &'static str,
    discard: Option<&'static str>,
    computed_points: f64,
    rule_points: f64,
}

#[allow(clippy::too_many_arguments)]
fn build(job: &Job, r: &JobResult, cfg: &Config, facts: &Facts, integrity: &Integrity, trace: (bool, Vec<Value>), bonus_label: Option<(&str, &str)>) -> Built {
    let route = &job.info.fingerprint.route;
    let names = &job.info.names;
    let d = &r.distances;
    let mut by_class = Map::new();
    for c in [ScaleClass::City, ScaleClass::Road, ScaleClass::OneToOne, ScaleClass::Nonstandard, ScaleClass::Unknown] {
        let g = d.game_km_by_scale.iter().find(|(k, _)| *k == c).map_or(0.0, |(_, v)| *v);
        let w = d.world_km_by_scale.iter().find(|(k, _)| *k == c).map_or(0.0, |(_, v)| *v);
        by_class.insert(
            class_name(c).into(),
            json!({"world_km": round2(w), "world_mi": mi(w), "game_km": round2(g), "game_mi": mi(g)}),
        );
    }
    let multiplayer = if r.truckersmp {
        let (server_id, account_id) = facts.truckersmp.unwrap_or((0, 0));
        json!({"kind": "truckersmp", "truckersmp": {"server_id": server_id, "account_id": account_id}})
    } else if r.convoy {
        json!({"kind": "convoy", "truckersmp": null})
    } else {
        json!({"kind": "singleplayer", "truckersmp": null})
    };
    let bonus = match (&r.bonus, bonus_label) {
        (Some(b), Some((id, label))) => json!({"bonus_id": id, "label": label,
            "kind": if b.kind == BonusKind::Flat { "flat" } else { "multiplier" }, "amount": b.amount}),
        _ => Value::Null,
    };
    let discarded = r.outcome == Outcome::Discarded;
    let (world_km, game_km) = if discarded { (0.0, 0.0) } else { (d.world_km, d.counted_game_km) };
    let summary = json!({
        "cargo": {"id": job.first_info.fingerprint.cargo.cargo_id, "name": job.first_info.cargo_name},
        "source": place(&route.source_city, &names.source_city, &route.source_company, &names.source_company),
        "destination": place(&route.destination_city, &names.destination_city, &route.destination_company, &names.destination_company),
        "market": route.job_market,
        "world_km": round2(world_km), "world_mi": mi(world_km),
        "game_km": round2(game_km), "game_mi": mi(game_km),
        "planned_distance_km": route.planned_distance_km,
        "speed_penalty_applied": r.speed.penalty,
        "bonus": bonus,
        "distance_by_scale_class": by_class,
        "multiplayer": multiplayer,
    });

    // Reasons: review reasons, then invalid and discard.
    let mut reasons: Vec<Value> = r.review.iter().map(|x| reason(x.code(), review_details(x, job, cfg))).collect();
    let (trace_ok, trace_list) = trace;
    if !trace_ok {
        let failed: Vec<Value> = trace_list.iter().filter(|c| c["ok"] == false).cloned().collect();
        reasons.push(reason("trace_check", json!({"checks": failed})));
    }
    let limits = cfg.rules.speed.limits(job.game);
    if r.speed.invalid {
        reasons.push(reason(
            "overspeed_invalid",
            json!({"peak_mph": round2(r.speed.peak_mph), "over_invalid_s": round2(r.speed.over_invalid_s), "invalid_over_mph": limits.invalid_over_mph}),
        ));
    }
    let discard = r.discard.as_ref().map(|x| match x {
        DiscardReason::Integrity => "integrity",
        DiscardReason::NegativeDistance => "negative_distance",
        DiscardReason::SingleplayerDivisionTruckersmp => "singleplayer_division_truckersmp",
    });
    if let Some(code) = discard {
        let details = if code == "integrity" {
            json!({"config_matched": integrity.json["config_matched_version"], "manifest_result": integrity.json["manifest_result"]})
        } else {
            json!({})
        };
        reasons.push(reason(code, details));
    }
    let auto_outcome = match r.outcome {
        Outcome::Valid if !trace_ok => "pending_review",
        o => outcome_str(o),
    };

    let segments = segments(job, facts);
    let refuels: Vec<Value> = r
        .refuels
        .iter()
        .map(|f| json!({"start_at": rfc3339(f.wall_ms), "end_at": rfc3339(f.wall_ms + (f.duration_s * 1000.0) as i64), "litres": round2(f.litres)}))
        .collect();
    let rules = json!([
        {"rule": "integrity", "triggered": discard == Some("integrity"), "details": {}},
        {"rule": "negative_distance", "triggered": discard == Some("negative_distance"), "details": {"negative_km": round2(d.negative_km)}},
        {"rule": "overspeed_invalid", "triggered": r.speed.invalid, "details": {"over_invalid_s": round2(r.speed.over_invalid_s)}},
        {"rule": "long_job", "triggered": r.review.iter().any(|x| x.code() == "long_job"), "details": {"world_km": round2(d.world_km)}},
        {"rule": "fuel", "triggered": r.review.iter().any(|x| matches!(x.code(), "negative_fuel" | "fuel_jump")), "details": {}},
        {"rule": "overspeed_penalty", "triggered": r.speed.penalty, "details": {"over_penalty_s": round2(r.speed.over_penalty_s)}},
        {"rule": "bonus", "triggered": r.bonus.is_some(), "details": {}},
    ]);
    let mut integ = integrity.json.clone();
    integ.as_object_mut().expect("object").remove("hellos");
    integ["trace_checks"] = Value::Array(trace_list);
    let detail = json!({
        "state": job.state,
        "start_kind": job.start,
        "profile_key": job.profile,
        "plugin_sessions": facts.sessions,
        "bonus_set": match bonus_label { Some((id, label)) => json!([{"bonus_id": id, "label": label, "applied": true}]), None => json!([]) },
        "distances": {
            "delivered_game_km": d.delivered_game_km,
            "tracked_whole_game_km": round2(d.tracked_whole_game_km),
            "tracked_counted_game_km": round2(d.tracked_counted_game_km),
            "tracked_uncounted_game_km": round2(d.tracked_uncounted_game_km),
            "counted_game_km": round2(d.counted_game_km),
            "world_km": util::round3(d.world_km),
            "world_mi": util::round3(d.world_mi),
            "accepted_world_mi": util::round3(r.accepted_world_mi),
        },
        "speed": {"peak_mph": round2(r.speed.peak_mph), "over_penalty_s": round2(r.speed.over_penalty_s),
                  "over_invalid_s": round2(r.speed.over_invalid_s),
                  "penalty_over_mph": limits.penalty_over_mph, "invalid_over_mph": limits.invalid_over_mph},
        "fuel": {"used_l": round2(fuel_used(job)), "refuels": refuels},
        "rules": rules,
        "integrity": integ,
        "segments": segments,
        "timeline": job.events.iter().map(event_json).collect::<Vec<_>>(),
    });
    let computed_points = if discarded { 0.0 } else { cfg.rules.floor_points(r.points_raw) };
    Built { summary, detail, reasons, auto_outcome, discard, computed_points, rule_points: r.points }
}

fn segments(job: &Job, facts: &Facts) -> Vec<Value> {
    let mut out = Vec::new();
    let mut run: Option<(u32, u64, u64, i64, i64)> = None; // sess, seq_from, seq_to, from, to
    let flush = |out: &mut Vec<Value>, r: Option<(u32, u64, u64, i64, i64)>| {
        if let Some((s, a, b, from, to)) = r {
            out.push(json!({"status": "kept", "from": rfc3339(from), "to": rfc3339(to), "sess": facts.sess_of(s), "seq_from": a, "seq_to": b}));
        }
    };
    for p in &job.kept {
        match &mut run {
            Some((s, _, b, _, to)) if *s == p.sess => {
                *b = p.seq;
                *to = p.wall_ms;
            }
            _ => {
                flush(&mut out, run.take());
                run = Some((p.sess, p.seq, p.seq, p.wall_ms, p.wall_ms));
            }
        }
    }
    flush(&mut out, run);
    for br in &job.reverted {
        if let (Some(a), Some(b)) = (br.points.first(), br.points.last()) {
            out.push(json!({"status": "reverted", "from": rfc3339(a.wall_ms), "to": rfc3339(b.wall_ms), "sess": facts.sess_of(a.sess),
                "seq_from": a.seq, "seq_to": b.seq, "by_load_at": rfc3339(br.reverted_at_wall_ms)}));
        }
    }
    for g in job.active_gaps() {
        out.push(json!({"status": "gap", "from": Value::Null, "to": rfc3339(g.wall_ms), "sess": facts.sess_of(g.to_sess),
            "seq_from": g.from_seq, "seq_to": g.to_seq, "kind": g.kind}));
    }
    out
}

// ---- persistence --------------------------------------------------------------------

pub fn etag(job_id: &str, version: i64) -> String {
    format!("\"j{job_id}-v{version}\"")
}

/// Points and held points for an outcome (PUBLIC_API §3).
pub fn points_for(outcome: &str, rule_points: f64, computed: f64) -> (Value, Value) {
    match outcome {
        "valid" => (json!(rule_points), Value::Null),
        "approved" => (json!(computed), Value::Null),
        "pending_review" => (Value::Null, json!(computed)),
        "incomplete" => (Value::Null, Value::Null),
        _ => (json!(0.0), Value::Null),
    }
}

/// Bonuses that may apply to a game (not archived), with their labels.
pub async fn bonuses<H: Host>(h: &H, game: Game) -> ApiResult<Vec<(Bonus, String)>> {
    Ok(h
        .all("SELECT * FROM bonuses WHERE game = ?1 AND archived_at IS NULL", params![game.as_str()])
        .await?
        .into_iter()
        .map(|r| {
            (
                Bonus {
                    id: r.s("bonus_id"),
                    game,
                    cargo_match: r.s("match"),
                    exact: r.s("match_kind") == "exact",
                    kind: if r.s("kind") == "flat" { BonusKind::Flat } else { BonusKind::Multiplier },
                    amount: r.f("amount"),
                    active_from_ms: Some(r.i("active_from")),
                    active_to_ms: Some(r.i("active_to")),
                },
                r.s("label"),
            )
        })
        .collect())
}

/// The Singleplayer Division flag as it was at `at_ms` (ADMIN_API §10).
pub fn division_at(user: &User, at_ms: i64) -> bool {
    if user.division_changed_at <= at_ms {
        user.singleplayer_division
    } else {
        !user.singleplayer_division
    }
}

/// Scores and stores one job; emits the webhooks its change calls for.
/// Returns the job id, or `None` for a job that was never delivered.
pub async fn persist<H: Host>(
    h: &H,
    cfg: &Config,
    config_version: Option<&str>,
    user: &User,
    job: &Job,
    facts: &Facts,
    trigger: Trigger,
) -> ApiResult<Option<String>> {
    let game = job.game;
    let existing = store::one(
        h,
        "SELECT * FROM jobs WHERE user_id = ?1 AND game = ?2 AND profile_key = ?3 AND local_id = ?4",
        params![&user.user_id, game.as_str(), &job.profile, job.id],
    )
    .await?;
    if job.delivered.is_none() && existing.is_none() {
        return Ok(None);
    }
    let now = h.now_ms();
    let delivered_at = facts.delivered_at.or_else(|| existing.as_ref().and_then(|r| r.opt_i("delivered_at")));
    let at = delivered_at.unwrap_or(now);
    let integrity = integrity(h, &user.user_id, &facts.sessions, at).await?;
    let bonus_rows = bonuses(h, game).await?;
    let ctx = Context {
        bonuses: bonus_rows.iter().map(|(b, _)| b.clone()).collect(),
        integrity_ok: Some(integrity.ok),
        singleplayer_division: division_at(user, at),
        delivered_at_ms: Some(at),
    };
    let r = evaluate(job, cfg, &ctx);
    let label = r.bonus.as_ref().and_then(|b| bonus_rows.iter().find(|(x, _)| x.id == b.id)).map(|(b, l)| (b.id.as_str(), l.as_str()));
    let trace = trace_checks(job, cfg, &integrity.json["hellos"]);
    let built = build(job, &r, cfg, facts, &integrity, trace, label);

    let old_outcome = existing.as_ref().map(|e| e.s("outcome"));
    let outcome = match &old_outcome {
        Some(o) if DECIDED.contains(&o.as_str()) => o.clone(),
        _ => built.auto_outcome.to_string(),
    };
    let (points, held) = points_for(&outcome, built.rule_points, built.computed_points);
    let reasons = Value::Array(built.reasons.clone());
    let tmp = facts.truckersmp.filter(|_| r.truckersmp);
    let kind = built.summary["multiplayer"]["kind"].as_str().unwrap_or("singleplayer").to_string();

    let job_id = match &existing {
        Some(e) => e.s("job_id"),
        None => store::new_job_id(h),
    };
    let changed = match &existing {
        None => true,
        Some(e) => {
            e.s("outcome") != outcome
                || e.json("summary") != built.summary
                || e.json("detail") != built.detail
                || e.opt_f("points") != points.as_f64()
                || e.opt_f("held_points") != held.as_f64()
                || e.s("auto_outcome") != built.auto_outcome
        }
    };
    if !changed {
        return Ok(Some(job_id));
    }
    let version = existing.as_ref().map_or(1, |e| e.i("version") + 1);
    let bonus_id = r.bonus.as_ref().map(|b| b.id.clone()).filter(|s| !s.is_empty());
    let started = job.events.first().map(|e| event_json(e)["at"].as_str().and_then(util::parse_rfc3339).unwrap_or(0)).unwrap_or(0);
    let p = params![
        &job_id,
        &user.user_id,
        game.as_str(),
        &job.profile,
        job.id,
        version,
        serde_json::to_value(job.state).unwrap_or_default().as_str().unwrap_or(""),
        serde_json::to_value(job.start).unwrap_or_default().as_str().unwrap_or(""),
        &outcome,
        built.auto_outcome,
        points.as_f64(),
        held.as_f64(),
        built.computed_points,
        built.summary["world_km"].as_f64().unwrap_or(0.0),
        built.summary["game_km"].as_f64().unwrap_or(0.0),
        &job.first_info.fingerprint.cargo.cargo_id,
        &job.info.fingerprint.route.job_market,
        reasons.to_string(),
        kind,
        tmp.map(|t| t.0),
        tmp.map(|t| t.1),
        bonus_id,
        config_version,
        delivered_at,
        job.delivered_wall_ms,
        started,
        built.summary.to_string(),
        built.detail.to_string(),
        now
    ];
    h.run(
        "INSERT INTO jobs (job_id, user_id, game, profile_key, local_id, version, state, start_kind, outcome, auto_outcome,
             points, held_points, computed_points, world_km, game_km, cargo_id, market, reasons, multiplayer,
             tmp_server_id, tmp_account_id, bonus_id, config_version_id, delivered_at, reported_delivered_at,
             started_wall_ms, summary, detail, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                 ?24, ?25, ?26, ?27, ?28, ?29)
         ON CONFLICT (job_id) DO UPDATE SET version = excluded.version, state = excluded.state,
             start_kind = excluded.start_kind, outcome = excluded.outcome, auto_outcome = excluded.auto_outcome,
             points = excluded.points, held_points = excluded.held_points, computed_points = excluded.computed_points,
             world_km = excluded.world_km, game_km = excluded.game_km, cargo_id = excluded.cargo_id,
             market = excluded.market, reasons = excluded.reasons, multiplayer = excluded.multiplayer,
             tmp_server_id = excluded.tmp_server_id, tmp_account_id = excluded.tmp_account_id,
             bonus_id = excluded.bonus_id, config_version_id = excluded.config_version_id,
             delivered_at = excluded.delivered_at, reported_delivered_at = excluded.reported_delivered_at,
             started_wall_ms = excluded.started_wall_ms, summary = excluded.summary, detail = excluded.detail,
             updated_at = excluded.updated_at",
        p,
    )
    .await?;
    let blob = json!({"job": job, "sess_ids": facts.sess_ids});
    h.blob_put(&blob_key(&job_id), util::gzip(blob.to_string().as_bytes())).await?;

    // Webhooks (WEBHOOKS §4).
    let points_changed = existing.as_ref().is_some_and(|e| e.opt_f("points") != points.as_f64() || e.opt_f("held_points") != held.as_f64());
    let outcome_changed = old_outcome.as_deref() != Some(outcome.as_str());
    if outcome == "incomplete" && !outcome_changed {
        return Ok(Some(job_id));
    }
    let player = user.brief();
    let delivered = util::opt_rfc3339(delivered_at);
    if outcome_changed && matches!(outcome.as_str(), "pending_review" | "invalid") {
        webhooks::emit(
            h,
            "job.needs_review",
            json!({"job_id": job_id, "game": game.as_str(), "job_version": version, "player": player,
                   "outcome": outcome, "held_points": held, "reasons": reasons, "delivered_at": delivered}),
            0,
        )
        .await?;
    } else if outcome_changed && outcome == "discarded" {
        webhooks::emit(
            h,
            "job.discarded",
            json!({"job_id": job_id, "game": game.as_str(), "job_version": version, "player": player,
                   "reasons": built.discard.map(|d| vec![d]).unwrap_or_default()}),
            0,
        )
        .await?;
    } else if outcome_changed || points_changed {
        let by = match trigger {
            Trigger::Recalculation => json!({"kind": "recalculation"}),
            _ => json!({"kind": "rules"}),
        };
        webhooks::emit(
            h,
            "job.scored",
            json!({"job_id": job_id, "game": game.as_str(), "job_version": version, "player": player,
                   "outcome": outcome, "previous_outcome": old_outcome, "points": points, "held_points": held, "by": by}),
            0,
        )
        .await?;
    }
    // The player's DM for an Invalid delivery (PLAN decision 16), after a
    // random 5-30 s delay.
    let was_invalid = existing.as_ref().is_some_and(|e| e.s("auto_outcome") == "invalid");
    if trigger == Trigger::Upload && built.auto_outcome == "invalid" && !was_invalid {
        notify(h, cfg, user, "invalid_delivery", Some(&job_id), game, job.delivered_wall_ms.unwrap_or(now), &job.info.cargo_name, &r).await?;
    }
    Ok(Some(job_id))
}

/// A `player.notification` webhook, delayed by a random 5-30 s.
#[allow(clippy::too_many_arguments)]
pub async fn notify<H: Host>(
    h: &H,
    cfg: &Config,
    user: &User,
    kind: &str,
    job_id: Option<&str>,
    game: Game,
    happened_ms: i64,
    cargo: &str,
    r: &JobResult,
) -> ApiResult<()> {
    if !cfg.notifications.enabled {
        return Ok(());
    }
    let limits = cfg.rules.speed.limits(game);
    let (title, text) = match kind {
        "invalid_delivery" => (
            "Invalid delivery".to_string(),
            format!(
                "{cargo}: you drove over {} mph (peak {:.0} mph), so this delivery scores no points. Moderators can still approve it.",
                limits.invalid_over_mph, r.speed.peak_mph
            ),
        ),
        _ => (
            "Speed penalty".to_string(),
            format!(
                "{cargo}: you drove over {} mph for too long. This job's distance now counts at {}×.",
                limits.penalty_over_mph, cfg.rules.speed_penalty_factor
            ),
        ),
    };
    let (lo, hi) = (cfg.notifications.delay_min_wall_s.ceil() as u32, cfg.notifications.delay_max_wall_s.floor() as u32);
    let mut b = [0u8; 4];
    h.random(&mut b);
    let delay = lo + u32::from_le_bytes(b) % (hi.saturating_sub(lo) + 1);
    webhooks::emit(
        h,
        "player.notification",
        json!({"recipient": {"user_id": user.user_id, "discord_user_id": user.discord_user_id},
               "kind": kind, "job_id": job_id, "game": game.as_str(), "happened_at": rfc3339(happened_ms),
               "title": title, "text": text}),
        delay,
    )
    .await?;
    Ok(())
}

// ---- reading ------------------------------------------------------------------------

/// The job summary (PUBLIC_API §3) of a `jobs` row.
pub fn summary_json(row: &Row) -> Value {
    let mut v = row.json("summary");
    let outcome = row.s("outcome");
    let o = v.as_object_mut().expect("summary is an object");
    let mut out = Map::new();
    out.insert("job_id".into(), row.s("job_id").into());
    out.insert("game".into(), row.s("game").into());
    out.insert("outcome".into(), outcome.clone().into());
    out.insert("points".into(), row.opt_f("points").map_or(Value::Null, Value::from));
    out.insert("held_points".into(), row.opt_f("held_points").map_or(Value::Null, Value::from));
    for k in ["world_km", "world_mi", "game_km", "game_mi", "cargo", "source", "destination", "market"] {
        out.insert(k.into(), o.remove(k).unwrap_or(Value::Null));
    }
    if matches!(outcome.as_str(), "discarded") {
        for k in ["world_km", "world_mi", "game_km", "game_mi"] {
            out.insert(k.into(), json!(0.0));
        }
    }
    out.insert("delivered_at".into(), util::opt_rfc3339(row.opt_i("delivered_at")));
    out.insert("reported_delivered_at".into(), util::opt_rfc3339(row.opt_i("reported_delivered_at")));
    out.insert("multiplayer".into(), o.remove("multiplayer").unwrap_or(Value::Null));
    Value::Object(out)
}

/// The public job detail: the summary plus player, bonus, scale classes, notes.
pub fn detail_json(row: &Row, user: &User, cfg: &Config) -> Value {
    let mut v = summary_json(row);
    let s = row.json("summary");
    let discard = row
        .json("reasons")
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|r| r["code"].as_str().filter(|c| matches!(*c, "integrity" | "negative_distance" | "singleplayer_division_truckersmp")).map(str::to_string));
    v["player"] = user.brief();
    v["planned_distance_km"] = s["planned_distance_km"].clone();
    v["speed_penalty_applied"] = s["speed_penalty_applied"].clone();
    v["bonus"] = s["bonus"].clone();
    v["distance_by_scale_class"] = s["distance_by_scale_class"].clone();
    v["public_notes"] = public_notes(&row.s("outcome"), &s, discard.as_deref(), cfg.rules.speed_penalty_factor).into();
    v
}
