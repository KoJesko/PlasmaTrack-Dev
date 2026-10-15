//! Moderator endpoints about jobs (ADMIN_API §1-8, §13) and the TruckersMP
//! flags (§3).

use pt_core::timeline::{Job, JobEvent};
use serde_json::{json, Map, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::{self, ApiError, ApiResult, Request, Response};
use crate::jobs::{self, Trigger};
use crate::public::{self, job_row};
use crate::scorer::Scorer;
use crate::store::{self, Audit, User};
use crate::util::{self, rfc3339};
use crate::{auth, config, manual, params, sheet, webhooks};

// ---- review queue (§1) -----------------------------------------------------------

fn queue_filter(req: &Request) -> ApiResult<(String, Vec<crate::env::P>)> {
    let mut sql = String::from("outcome IN (");
    let outcomes: Vec<&str> = match req.q("outcome") {
        Some(s) if !s.is_empty() => s.split(',').map(str::trim).collect(),
        _ => vec!["pending_review", "invalid"],
    };
    if outcomes.iter().any(|o| !matches!(*o, "pending_review" | "invalid")) {
        return Err(ApiError::bad_request("outcome is pending_review and/or invalid"));
    }
    sql.push_str(&outcomes.iter().map(|o| format!("'{o}'")).collect::<Vec<_>>().join(","));
    sql.push(')');
    let mut p = Vec::new();
    if let Some(g) = req.q("game") {
        public::game(g)?;
        p.push(g.into());
        sql.push_str(&format!(" AND game = ?{}", p.len()));
    }
    if let Some(code) = req.q("reason") {
        if !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(ApiError::bad_request("bad reason code"));
        }
        p.push(format!("%\"code\":\"{code}\"%").into());
        sql.push_str(&format!(" AND reasons LIKE ?{}", p.len()));
    }
    Ok((sql, p))
}

pub async fn review_queue<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let (filter, p) = queue_filter(req)?;
    let total = store::count(h, &format!("SELECT COUNT(*) AS n FROM jobs WHERE {filter}"), p.clone()).await?;
    let mut p2 = p;
    p2.push((limit as i64).into());
    p2.push((offset as i64).into());
    let n = p2.len();
    let rows = h
        .all(
            &format!("SELECT * FROM jobs WHERE {filter} ORDER BY delivered_at, job_id LIMIT ?{} OFFSET ?{}", n - 1, n),
            p2,
        )
        .await?;
    let mut items = Vec::new();
    for r in &rows {
        let user = store::user_by_id(h, &r.s("user_id")).await?.ok_or_else(|| ApiError::internal("job without user"))?;
        let s = r.json("summary");
        let brief = |p: &Value| json!({"city_id": p["city_id"], "city_name": p["city_name"]});
        items.push(json!({
            "job_id": r.s("job_id"), "game": r.s("game"), "origin": r.s("origin"), "outcome": r.s("outcome"), "player": user.brief(),
            "held_points": r.opt_f("held_points"), "world_km": s["world_km"], "game_km": s["game_km"],
            "cargo": s["cargo"], "source": brief(&s["source"]), "destination": brief(&s["destination"]),
            "delivered_at": util::opt_rfc3339(r.opt_i("delivered_at")),
            "age_s": r.opt_i("delivered_at").map(|d| (now - d) / 1000),
            "reasons": r.json("reasons"),
        }));
    }
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, now)))
}

pub async fn review_queue_count<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let (filter, p) = match req.q("game") {
        Some(g) => {
            public::game(g)?;
            ("AND game = ?1", vec![g.into()])
        }
        None => ("", vec![]),
    };
    let rows = h.all(&format!("SELECT outcome, reasons FROM jobs WHERE outcome IN ('pending_review', 'invalid') {filter}"), p).await?;
    let mut by_reason = Map::new();
    for code in ["late_start", "distance_mismatch", "long_job", "fuel_jump"] {
        by_reason.insert(code.into(), json!(0));
    }
    let (mut pending, mut invalid) = (0, 0);
    for r in &rows {
        if r.s("outcome") == "invalid" {
            invalid += 1;
            continue;
        }
        pending += 1;
        for reason in r.json("reasons").as_array().into_iter().flatten() {
            if let Some(code) = reason["code"].as_str() {
                let e = by_reason.entry(code.to_string()).or_insert(json!(0));
                *e = json!(e.as_i64().unwrap_or(0) + 1);
            }
        }
    }
    Ok(Response::ok(json!({"pending_review": pending, "invalid": invalid, "by_reason": by_reason})))
}

// ---- job detail (§4) -------------------------------------------------------------

async fn audit_items<H: Host>(h: &H, rows: &[Row]) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    for r in rows {
        let actor = r.s("actor");
        let name = match actor.as_str() {
            "self" | "system" => Value::Null,
            id => store::user_by_discord(h, id).await?.map_or(Value::Null, |u| u.display_name.into()),
        };
        out.push(json!({
            "audit_id": format!("aud_{}", r.i("audit_no")), "at": rfc3339(r.i("at")),
            "actor": {"discord_user_id": actor, "display_name": name},
            "action": r.s("action"), "target": {"kind": r.s("target_kind"), "id": r.s("target_id")},
            "reason": r.opt_s("reason"), "before": r.json("before"), "after": r.json("after"),
        }));
    }
    Ok(out)
}

pub async fn job_detail<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let row = job_row(h, raw_id).await?;
    let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("job"))?;
    let cfg = config::scoring(h).await?.cfg;
    let mut v = jobs::detail_json(&row, &user, &cfg);
    let job_id = row.s("job_id");
    v["reasons"] = row.json("reasons");
    v["etag"] = jobs::etag(&job_id, row.i("version")).into();
    if let Value::Object(d) = row.json("detail") {
        for (k, x) in d {
            v[k.as_str()] = x;
        }
    }
    v["config_version"] = match row.opt_s("config_version_id") {
        Some(id) => {
            let c = store::one(h, "SELECT integrity_hash, current FROM config_versions WHERE version_id = ?1", params![&id]).await?;
            json!({"version_id": id, "integrity_hash": c.as_ref().map(|c| c.s("integrity_hash")), "current": c.is_some_and(|c| c.b("current"))})
        }
        None => Value::Null,
    };
    let audit_rows =
        h.all("SELECT * FROM audit_log WHERE target_kind = 'job' AND target_id = ?1 ORDER BY audit_no DESC", params![&job_id]).await?;
    v["audit"] = audit_items(h, &audit_rows).await?.into();
    manual::admin_extras(h, &row, &mut v).await?;
    v["custom_map"] = manual::custom_map(h, &row, true).await?;
    Ok(Response::ok(v))
}

// ---- edits (§14) -----------------------------------------------------------------

fn check_etag(body: &Value, job_id: &str, version: i64) -> ApiResult<()> {
    if let Some(expected) = body.get("expected_etag").and_then(Value::as_str) {
        if expected != jobs::etag(job_id, version) {
            return Err(ApiError::conflict("The job changed; reload it.", json!({"current_etag": jobs::etag(job_id, version)})));
        }
    }
    Ok(())
}

fn snapshot(r: &Row) -> Value {
    json!({"outcome": r.s("outcome"), "points": r.opt_f("points"), "held_points": r.opt_f("held_points")})
}

/// Applies a moderator's field changes to a job and re-scores it. Returns the
/// reloaded row.
async fn apply_edit<H: Host>(h: &H, admin: &User, row: Row, fields: &Value, reason: &str, quiet: bool) -> ApiResult<Row> {
    let job_id = row.s("job_id");
    if !fields.as_object().is_some_and(|o| o.keys().any(|k| sheet::EDIT_KEYS.contains(&k.as_str()))) {
        return Err(ApiError::field("fields", "nothing to change"));
    }
    let mut row = row;
    if row.s("origin") == "tracked" && row.json("detail")["sheet_base"].is_null() {
        // Scored before edits existed: score it once so the recording's sheet is stored.
        Scorer::rescore_job(h, &job_id, Trigger::Recalculation, None).await?;
        row = job_row(h, &job_id).await?;
    }
    let before = snapshot(&row);
    let (o_before, o_after) = manual::write_overrides(h, &row, fields).await?;
    let m = jobs::Moderator { discord_user_id: &admin.discord_user_id, quiet };
    Scorer::rescore_job(h, &job_id, Trigger::Edit, Some(m)).await?;
    let now = job_row(h, &job_id).await?;
    let (mut b, mut a) = (before, snapshot(&now));
    b["overrides"] = sheet::to_json(&serde_json::from_value(o_before).unwrap_or_default());
    a["overrides"] = sheet::to_json(&serde_json::from_value(o_after).unwrap_or_default());
    let user = store::user_by_id(h, &now.s("user_id")).await?;
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "job.edit", target_kind: "job", target_id: &job_id,
        target_user: user.as_ref().map(|u| u.discord_user_id.as_str()), reason: Some(reason), before: b, after: a,
    })
    .await?;
    Ok(now)
}

/// `PATCH /api/v1/admin/jobs/{job_id}`: correct any field of any job.
pub async fn edit<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    let admin = auth::admin(h, req).await?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let body = req.json_body()?;
    let reason = store::reason(&body)?;
    sheet::check_keys(&body, &["reason", "expected_etag"])?;
    let row = job_row(h, raw_id).await?;
    let job_id = row.s("job_id");
    check_etag(&body, &job_id, row.i("version"))?;
    let now = apply_edit(h, &admin, row, &body, &reason, false).await?;
    let overrides: serde_json::Map<String, Value> = serde_json::from_value(now.json("overrides")).unwrap_or_default();
    let resp = json!({"job_id": job_id, "outcome": now.s("outcome"), "points": now.opt_f("points"), "held_points": now.opt_f("held_points"),
                      "overrides": sheet::to_json(&overrides), "etag": jobs::etag(&job_id, now.i("version"))});
    store::idem_store(h, idem, 200, &resp).await?;
    Ok(Response::ok(resp))
}

// ---- decisions (§7) --------------------------------------------------------------

pub async fn decide<H: Host>(h: &H, req: &Request, raw_id: &str, action: &str) -> ApiResult<Response> {
    let admin = auth::admin(h, req).await?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let body = req.json_body()?;
    let reason = store::reason(&body)?;
    let mut row = job_row(h, raw_id).await?;
    let job_id = row.s("job_id");
    check_etag(&body, &job_id, row.i("version"))?;
    let corrections = body.get("corrections").filter(|c| !c.is_null());
    let hide_map = match body.get("hide_map") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(ApiError::field("hide_map", "true or false")),
    };
    if action != "approve" && (corrections.is_some() || hide_map.is_some()) {
        return Err(ApiError::field(if corrections.is_some() { "corrections" } else { "hide_map" }, "only with approve"));
    }
    let from = row.s("outcome");
    let to = match (action, from.as_str()) {
        ("approve", "pending_review" | "invalid" | "rejected") => "approved".to_string(),
        ("reject", "pending_review" | "valid" | "approved" | "invalid") => "rejected".to_string(),
        ("void", f) if !matches!(f, "discarded" | "voided") => "voided".to_string(),
        ("restore", "voided") => row.opt_s("pre_void_outcome").unwrap_or_else(|| row.s("auto_outcome")),
        _ => {
            return Err(ApiError::invalid_transition(&from, action, &format!("A {from} job cannot be {action}d.")));
        }
    };
    // Approval may correct stats in the same step (§7, §14); the edit sends no
    // webhook of its own, the approval's job.scored carries the result.
    let mut extra = json!({});
    if let Some(c) = corrections {
        if !c.is_object() {
            return Err(ApiError::field("corrections", "an object"));
        }
        sheet::check_keys(c, &[])?;
        row = apply_edit(h, &admin, row, c, &reason, true).await?;
        extra["corrections"] = row.json("overrides");
    }
    if let Some(hm) = hide_map {
        h.run("UPDATE jobs SET hide_map = ?2 WHERE job_id = ?1", params![&job_id, hm]).await?;
        extra["hide_map"] = hm.into();
    }
    let version = row.i("version");
    let computed = row.f("computed_points");
    let (points, held) = jobs::points_for(&to, computed, computed);
    let pre_void = if to == "voided" { Some(from.clone()) } else { None };
    let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::internal("job without user"))?;
    let new_version = version + 1;
    let before = json!({"outcome": from, "points": row.opt_f("points"), "held_points": row.opt_f("held_points")});
    let mut after = json!({"outcome": to, "points": points, "held_points": held});
    for (k, v) in extra.as_object().into_iter().flatten() {
        after[k.as_str()] = v.clone();
    }
    h.batch(vec![
        (
            "UPDATE jobs SET outcome = ?2, points = ?3, held_points = ?4, version = ?5, updated_at = ?6,
                 pre_void_outcome = COALESCE(?7, pre_void_outcome) WHERE job_id = ?1"
                .into(),
            params![&job_id, &to, points.as_f64(), held.as_f64(), new_version, h.now_ms(), pre_void],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: &format!("job.{action}"), target_kind: "job", target_id: &job_id,
            target_user: Some(&user.discord_user_id), reason: Some(&reason), before, after,
        }),
    ])
    .await?;
    webhooks::emit(
        h,
        "job.scored",
        json!({"job_id": job_id, "game": row.s("game"), "job_version": new_version, "player": user.brief(),
               "outcome": to, "previous_outcome": from, "points": points, "held_points": held,
               "by": {"kind": "moderator", "discord_user_id": admin.discord_user_id}}),
        0,
    )
    .await?;
    let resp = json!({"job_id": job_id, "outcome": to, "points": points, "held_points": held, "etag": jobs::etag(&job_id, new_version)});
    store::idem_store(h, idem, 200, &resp).await?;
    Ok(Response::ok(resp))
}

// ---- recalculation (§8) ----------------------------------------------------------

pub async fn recalculate_job<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    let admin = auth::admin(h, req).await?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let reason = store::reason(&req.json_body()?)?;
    let row = job_row(h, raw_id).await?;
    let job_id = row.s("job_id");
    let snap = |r: &Row| json!({"outcome": r.s("outcome"), "points": r.opt_f("points"), "held_points": r.opt_f("held_points")});
    let before = snap(&row);
    Scorer::rescore_job(h, &job_id, Trigger::Recalculation, None).await?;
    let now_row = store::one(h, "SELECT * FROM jobs WHERE job_id = ?1", params![&job_id]).await?.ok_or_else(|| ApiError::not_found("job"))?;
    let after = snap(&now_row);
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "job.recalculate", target_kind: "job", target_id: &job_id,
        target_user: None, reason: Some(&reason), before: before.clone(), after: after.clone(),
    })
    .await?;
    let resp = json!({"job_id": job_id, "before": before, "after": after, "etag": jobs::etag(&job_id, now_row.i("version"))});
    store::idem_store(h, idem, 200, &resp).await?;
    Ok(Response::ok(resp))
}

// ---- series (§5) -----------------------------------------------------------------

/// Bucket start, speed min and max, fuel min and max.
type Bucket = (i64, f64, f64, Option<f64>, Option<f64>);

pub async fn load_job<H: Host>(h: &H, job_id: &str) -> ApiResult<(Job, Vec<(u32, String)>)> {
    let blob = h.blob_get(&jobs::blob_key(job_id)).await?.ok_or_else(|| ApiError::not_found("job data"))?;
    let v: Value = serde_json::from_slice(&util::gunzip(&blob, usize::MAX).map_err(|e| ApiError::internal(&e.to_string()))?)
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    let job = serde_json::from_value(v["job"].clone()).map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok((job, serde_json::from_value(v["sess_ids"].clone()).unwrap_or_default()))
}

pub async fn series<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let max_points = match req.q("max_points") {
        None => 1000,
        Some(s) => s.parse::<usize>().ok().filter(|n| (1..=4000).contains(n)).ok_or_else(|| ApiError::bad_request("max_points is 1..4000"))?,
    };
    let row = job_row(h, raw_id).await?;
    let (job, _) = load_job(h, &row.s("job_id")).await?;
    let cfg = config::scoring(h).await?.cfg;
    let limits = cfg.rules.speed.limits(job.game);
    let (Some(first), Some(last)) = (job.kept.first(), job.kept.last()) else {
        return Ok(Response::ok(json!({"job_id": row.s("job_id"), "game": row.s("game"), "bucket_s": 0.0,
            "penalty_over_mph": limits.penalty_over_mph, "invalid_over_mph": limits.invalid_over_mph,
            "points": [], "windows": [], "markers": []})));
    };
    let span = (last.wall_ms - first.wall_ms).max(1);
    let bucket_ms = (span as f64 / max_points as f64).max(1.0);
    let mut buckets: Vec<Option<Bucket>> = vec![None; max_points];
    for p in &job.kept {
        let i = (((p.wall_ms - first.wall_ms) as f64 / bucket_ms) as usize).min(max_points - 1);
        let mph = p.mph();
        let b = buckets[i].get_or_insert((first.wall_ms + (i as f64 * bucket_ms) as i64, mph, mph, p.fuel_l, p.fuel_l));
        b.1 = b.1.min(mph);
        b.2 = b.2.max(mph);
        if let Some(f) = p.fuel_l {
            b.3 = Some(b.3.map_or(f, |x| x.min(f)));
            b.4 = Some(b.4.map_or(f, |x| x.max(f)));
        }
    }
    let r2 = util::round2;
    let points: Vec<Value> = buckets
        .into_iter()
        .flatten()
        .map(|(t, lo, hi, flo, fhi)| {
            json!({"t": rfc3339(t), "speed_mph_min": r2(lo), "speed_mph_max": r2(hi),
                   "fuel_l_min": flo.map(r2), "fuel_l_max": fhi.map(r2), "reverted": false})
        })
        .collect();
    let mut windows = Vec::new();
    for w in job.kept.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let wall = b.wall_ms - a.wall_ms;
        let sim = b.sim_us.saturating_sub(a.sim_us) as i64 / 1000;
        if a.sess == b.sess && wall - sim > 5_000 {
            windows.push(json!({"kind": "pause", "from": rfc3339(a.wall_ms), "to": rfc3339(b.wall_ms)}));
        }
    }
    for e in &job.events {
        if let JobEvent::Teleport { ferry_or_train: true, wall_ms, via, .. } = e {
            let from = job.kept.iter().rev().find(|p| p.wall_ms < *wall_ms).map_or(*wall_ms, |p| p.wall_ms);
            let kind = via.as_deref().unwrap_or("ferry_or_train");
            windows.push(json!({"kind": kind, "from": rfc3339(from), "to": rfc3339(*wall_ms)}));
        }
    }
    for br in &job.reverted {
        if let (Some(a), Some(b)) = (br.points.first(), br.points.last()) {
            windows.push(json!({"kind": "reverted", "from": rfc3339(a.wall_ms), "to": rfc3339(b.wall_ms)}));
        }
    }
    let result = pt_core::rules::evaluate(&job, &cfg, &Default::default());
    for f in &result.refuels {
        windows.push(json!({"kind": "refuel", "from": rfc3339(f.wall_ms),
            "to": rfc3339(f.wall_ms + (f.duration_s * 1000.0) as i64), "litres": r2(f.litres)}));
    }
    let markers: Vec<Value> = result
        .review
        .iter()
        .filter_map(|x| match x {
            pt_core::rules::ReviewReason::FuelJump { wall_ms, litres } => {
                Some(json!({"kind": "fuel_jump", "at": rfc3339(*wall_ms), "litres": r2(*litres)}))
            }
            _ => None,
        })
        .collect();
    Ok(Response::ok(json!({
        "job_id": row.s("job_id"), "game": row.s("game"), "bucket_s": r2(bucket_ms / 1000.0),
        "penalty_over_mph": limits.penalty_over_mph, "invalid_over_mph": limits.invalid_over_mph,
        "points": points, "windows": windows, "markers": markers,
    })))
}

// ---- trace (§6) ------------------------------------------------------------------

pub const TRACE_URL_TTL_MS: i64 = 15 * 60_000;

fn trace_sig<H: Host>(h: &H, job_id: &str, version: i64, exp_s: i64) -> String {
    util::hex(&util::hmac_sha256(h.settings().download_secret.as_bytes(), format!("trace.{job_id}.{version}.{exp_s}").as_bytes()))
}

fn trace_key(job_id: &str, version: i64) -> String {
    format!("traces/{job_id}-v{version}.jsonl.gz")
}

pub async fn trace<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    let admin = auth::admin(h, req).await?;
    let row = job_row(h, raw_id).await?;
    let job_id = row.s("job_id");
    let version = row.i("version");
    let key = trace_key(&job_id, version);
    let (size, frames) = match h.blob_get(&format!("{key}.meta")).await? {
        Some(m) => {
            let v: Value = serde_json::from_slice(&m).unwrap_or_default();
            (v["size"].as_i64().unwrap_or(0), v["frames"].as_i64().unwrap_or(0))
        }
        None => build_trace(h, &row, &key).await?,
    };
    let now = h.now_ms();
    let exp_s = (now + TRACE_URL_TTL_MS) / 1000;
    let url = format!(
        "{}/dl/trace/{job_id}?v={version}&exp={exp_s}&sig={}",
        h.settings().public_url.trim_end_matches('/'),
        trace_sig(h, &job_id, version, exp_s)
    );
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "trace.download", target_kind: "job", target_id: &job_id,
        target_user: None, reason: None, before: json!({}), after: json!({"version": version}),
    })
    .await?;
    Ok(Response::ok(json!({"url": url, "expires_at": rfc3339(exp_s * 1000), "size_bytes": size, "format": "jsonl+gzip", "frames": frames})))
}

/// Every raw frame of the job's sessions from accept to end, each marked
/// `pt_status` kept / reverted / outside.
async fn build_trace<H: Host>(h: &H, row: &Row, key: &str) -> ApiResult<(i64, i64)> {
    let (job, sess_ids) = load_job(h, &row.s("job_id")).await?;
    let wall = |e: &JobEvent| serde_json::to_value(e).ok().and_then(|v| v["wall_ms"].as_i64()).unwrap_or(0);
    let start = job.events.first().map(wall).unwrap_or(0);
    let end = job.events.last().map(wall).unwrap_or(i64::MAX);
    let id_of = |i: u32| sess_ids.iter().find(|(x, _)| *x == i).map(|(_, s)| s.clone()).unwrap_or_default();
    let kept: std::collections::HashSet<(String, u64)> = job.kept.iter().map(|p| (id_of(p.sess), p.seq)).collect();
    let reverted: std::collections::HashSet<(String, u64)> =
        job.reverted.iter().flat_map(|b| b.points.iter()).map(|p| (id_of(p.sess), p.seq)).collect();
    let user_id = row.s("user_id");
    let mut out = Vec::new();
    let mut frames = 0i64;
    for (_, sess) in &sess_ids {
        let chunks = h
            .all("SELECT r2_key FROM raw_chunks WHERE user_id = ?1 AND sess = ?2 ORDER BY chunk_no", params![&user_id, sess])
            .await?;
        for c in chunks {
            let Some(blob) = h.blob_get(&c.s("r2_key")).await? else { continue };
            let text = util::gunzip(&blob, usize::MAX).map_err(|e| ApiError::internal(&e.to_string()))?;
            for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                let Ok(mut v) = serde_json::from_slice::<Value>(line) else { continue };
                let w = v["wall_ms"].as_i64().unwrap_or(0);
                if w < start || w > end {
                    continue;
                }
                let k = (sess.clone(), v["seq"].as_u64().unwrap_or(0));
                v["pt_status"] = if kept.contains(&k) {
                    "kept"
                } else if reverted.contains(&k) {
                    "reverted"
                } else {
                    "outside"
                }
                .into();
                out.extend_from_slice(v.to_string().as_bytes());
                out.push(b'\n');
                frames += 1;
            }
        }
    }
    let z = util::gzip(&out);
    let size = z.len() as i64;
    h.blob_put(key, z).await?;
    h.blob_put(&format!("{key}.meta"), json!({"size": size, "frames": frames}).to_string().into_bytes()).await?;
    Ok((size, frames))
}

/// `GET /dl/trace/{job_id}?v=&exp=&sig=`: the signed download itself.
pub async fn download_trace<H: Host>(h: &H, req: &Request, job_id: &str) -> ApiResult<Response> {
    let (Some(v), Some(exp), Some(sig)) = (req.q("v"), req.q("exp"), req.q("sig")) else {
        return Err(ApiError::forbidden("This link is not valid."));
    };
    let (Ok(version), Ok(exp_s)) = (v.parse::<i64>(), exp.parse::<i64>()) else {
        return Err(ApiError::forbidden("This link is not valid."));
    };
    if !util::constant_eq(sig.as_bytes(), trace_sig(h, job_id, version, exp_s).as_bytes()) {
        return Err(ApiError::forbidden("This link is not valid."));
    }
    if h.now_ms() / 1000 > exp_s {
        return Err(ApiError::forbidden("This link has expired."));
    }
    let blob = h.blob_get(&trace_key(job_id, version)).await?.ok_or_else(|| ApiError::not_found("trace"))?;
    Ok(Response::bytes(200, "application/gzip", blob)
        .with_header("content-disposition", &format!("attachment; filename=\"plasmatrack-{job_id}-v{version}.jsonl.gz\"")))
}

// ---- audit (§13) -----------------------------------------------------------------

pub async fn audit_log<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let mut sql = String::from("1 = 1");
    let mut p: Vec<crate::env::P> = Vec::new();
    let mut add = |sql: &mut String, cond: &str, v: crate::env::P| {
        p.push(v);
        sql.push_str(&format!(" AND {cond} ?{}", p.len()));
    };
    if let Some(j) = req.q("job_id") {
        let id = util::normalize_job_id(j).ok_or_else(|| ApiError::bad_request("bad job_id"))?;
        sql.push_str(" AND target_kind = 'job'");
        add(&mut sql, "target_id =", id.into());
    }
    if let Some(a) = req.q("actor") {
        add(&mut sql, "actor =", a.into());
    }
    if let Some(t) = req.q("target_user") {
        add(&mut sql, "target_user =", t.into());
    }
    if let Some(a) = req.q("action") {
        add(&mut sql, "action =", a.into());
    }
    for (q, op) in [("since", "at >="), ("until", "at <")] {
        if let Some(s) = req.q(q) {
            let t = util::parse_rfc3339(s).ok_or_else(|| ApiError::bad_request(&format!("{q} must be RFC 3339")))?;
            add(&mut sql, op, t.into());
        }
    }
    let mut p2 = p.clone();
    p2.push(((limit + 1) as i64).into());
    p2.push((offset as i64).into());
    let n = p2.len();
    let rows = h
        .all(&format!("SELECT * FROM audit_log WHERE {sql} ORDER BY audit_no DESC LIMIT ?{} OFFSET ?{}", n - 1, n), p2)
        .await?;
    let more = rows.len() > limit;
    let items = audit_items(h, &rows[..rows.len().min(limit)]).await?;
    Ok(Response::ok(http::list(items, offset, limit, None, more, now)))
}

// ---- TruckersMP flags (§3) -------------------------------------------------------

pub async fn truckersmp_flags<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let flags = player_flags(h, None).await?;
    let total = flags.len();
    let items: Vec<Value> = flags.into_iter().skip(offset).take(limit).collect();
    Ok(Response::ok(http::list(items, offset, limit, None, offset + limit < total, now)))
}

/// Both kinds of flag, optionally only those involving one user.
pub async fn player_flags<H: Host>(h: &H, only: Option<&User>) -> ApiResult<Vec<Value>> {
    let rows = h.all("SELECT a.user_id, u.discord_user_id, a.account_id FROM truckersmp_accounts a JOIN users u ON u.user_id = a.user_id WHERE a.account_id > 0 ORDER BY a.user_id, a.account_id", vec![]).await?;
    let mut out = Vec::new();
    let mut by_user: std::collections::BTreeMap<(String, String), Vec<i64>> = Default::default();
    let mut by_account: std::collections::BTreeMap<i64, Vec<(String, String)>> = Default::default();
    for r in &rows {
        by_user.entry((r.s("user_id"), r.s("discord_user_id"))).or_default().push(r.i("account_id"));
        by_account.entry(r.i("account_id")).or_default().push((r.s("user_id"), r.s("discord_user_id")));
    }
    let wanted = |uid: &str| only.is_none_or(|u| u.user_id == uid);
    for ((uid, did), accounts) in by_user {
        if accounts.len() > 1 && wanted(&uid) {
            out.push(json!({"kind": "player_multiple_accounts", "user_id": uid, "discord_user_id": did, "account_ids": accounts}));
        }
    }
    for (account, users) in by_account {
        if users.len() > 1 && users.iter().any(|(u, _)| wanted(u)) {
            let users: Vec<Value> = users.iter().map(|(u, d)| json!({"user_id": u, "discord_user_id": d})).collect();
            out.push(json!({"kind": "account_multiple_players", "account_id": account, "users": users}));
        }
    }
    Ok(out)
}
