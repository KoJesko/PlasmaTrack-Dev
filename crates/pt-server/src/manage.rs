//! Moderator endpoints that manage things (ADMIN_API §8-12) and the
//! background work they start: bulk recalculation steps and the cron job.

use pt_core::Config;
use serde_json::{json, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::{self, ApiError, ApiResult, Request, Response};
use crate::jobs::Trigger;

use crate::store::{self, Audit, User};
use crate::util::{self, rfc3339};
use crate::{admin, auth, config, params, public, webhooks};

/// Shared start of every admin write: the actor, the idempotency record (or
/// the replayed answer) and the body with its reason.
async fn begin<H: Host>(h: &H, req: &Request) -> ApiResult<Result<(User, store::Idem, Value, String), Response>> {
    let admin = auth::admin(h, req).await?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(Err(replay)),
    };
    let body = req.json_body()?;
    let reason = store::reason(&body)?;
    Ok(Ok((admin, idem, body, reason)))
}

macro_rules! begin {
    ($h:expr, $req:expr) => {
        match begin($h, $req).await? {
            Ok(x) => x,
            Err(replay) => return Ok(replay),
        }
    };
}

async fn done<H: Host>(h: &H, idem: store::Idem, status: u16, v: Value) -> ApiResult<Response> {
    store::idem_store(h, idem, status, &v).await?;
    Ok(Response::json(status, &v))
}

// ---- bulk recalculation (§8) -----------------------------------------------------

const OP_BATCH: i64 = 50;

pub async fn recalculations<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let admin = auth::admin(h, req).await?;
    let body = req.json_body()?;
    let game = match body.get("game").and_then(Value::as_str) {
        Some(g) => Some(public::game(g)?.as_str()),
        None => None,
    };
    let (filter, p) = match game {
        Some(g) => ("AND game = ?1", params![g]),
        None => ("", vec![]),
    };
    let affected = store::count(h, &format!("SELECT COUNT(*) AS n FROM jobs WHERE outcome != 'incomplete' {filter}"), p).await?;
    if body.get("dry_run").and_then(Value::as_bool) == Some(true) {
        auth::bot(h, req)?;
        return Ok(Response::ok(json!({"affected_jobs": affected, "game": game})));
    }
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let reason = store::reason(&body)?;
    if let Some(r) = store::one(h, "SELECT operation_id FROM operations WHERE kind = 'recalculation' AND status IN ('queued', 'running')", vec![]).await? {
        return Err(ApiError::conflict("A recalculation is already running.", json!({"operation_id": r.s("operation_id")})));
    }
    let op = store::new_id(h, "op");
    h.batch(vec![
        (
            "INSERT INTO operations (operation_id, kind, status, game, total, requested_by, reason, started_at)
             VALUES (?1, 'recalculation', 'queued', ?2, ?3, ?4, ?5, ?6)"
                .into(),
            params![&op, game, affected as i64, &admin.discord_user_id, &reason, h.now_ms()],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "recalculation.start", target_kind: "operation", target_id: &op,
            target_user: None, reason: Some(&reason), before: json!({}), after: json!({"game": game, "affected_jobs": affected}),
        }),
    ])
    .await?;
    h.queue_op(json!({"kind": "recalculation", "operation_id": op})).await?;
    done(h, idem, 202, json!({"operation_id": op, "affected_jobs": affected})).await
}

pub async fn operation<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let r = store::one(h, "SELECT * FROM operations WHERE operation_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("operation"))?;
    Ok(Response::ok(operation_json(&r)))
}

fn operation_json(r: &Row) -> Value {
    let finished = r.opt_i("finished_at");
    json!({"operation_id": r.s("operation_id"), "kind": r.s("kind"), "status": r.s("status"),
           "progress": {"done": r.i("done"), "total": r.i("total")}, "started_at": rfc3339(r.i("started_at")),
           "finished_at": util::opt_rfc3339(finished), "summary": if finished.is_some() { r.json("summary") } else { Value::Null }})
}

/// One queue step of background work. Returns the follow-up message, if any.
pub async fn run_op<H: Host>(h: &H, msg: &Value) -> ApiResult<Option<Value>> {
    match msg["kind"].as_str() {
        Some("recalculation") => recalculation_step(h, msg["operation_id"].as_str().unwrap_or_default()).await,
        Some("poke_all") => {
            for r in h.all("SELECT DISTINCT user_id, game FROM plugin_sessions WHERE game IS NOT NULL", vec![]).await? {
                h.poke_scorer(&r.s("user_id"), &r.s("game")).await?;
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

async fn recalculation_step<H: Host>(h: &H, op: &str) -> ApiResult<Option<Value>> {
    let Some(r) = store::one(h, "SELECT * FROM operations WHERE operation_id = ?1", params![op]).await? else { return Ok(None) };
    if !matches!(r.s("status").as_str(), "queued" | "running") {
        return Ok(None);
    }
    let done_n = r.i("done");
    let (filter, mut p) = match r.opt_s("game") {
        Some(g) => ("AND game = ?1", params![g]),
        None => ("", vec![]),
    };
    p.push(OP_BATCH.into());
    p.push(done_n.into());
    let n = p.len();
    let rows = h
        .all(&format!("SELECT job_id, outcome FROM jobs WHERE outcome != 'incomplete' {filter} ORDER BY job_id LIMIT ?{} OFFSET ?{}", n - 1, n), p)
        .await?;
    let mut summary: serde_json::Map<String, Value> = serde_json::from_value(r.json("summary")).unwrap_or_default();
    for j in &rows {
        let id = j.s("job_id");
        let before = j.s("outcome");
        if crate::scorer::rescore_job(h, &id, Trigger::Recalculation, None).await.is_err() {
            continue;
        }
        let after = store::one(h, "SELECT outcome FROM jobs WHERE job_id = ?1", params![&id]).await?.map(|x| x.s("outcome")).unwrap_or_default();
        if after != before {
            let k = format!("{before}→{after}");
            let e = summary.entry(k).or_insert(json!(0));
            *e = json!(e.as_i64().unwrap_or(0) + 1);
        }
    }
    let new_done = done_n + rows.len() as i64;
    let finished = (rows.len() as i64) < OP_BATCH;
    let now = h.now_ms();
    h.run(
        "UPDATE operations SET status = ?2, done = ?3, summary = ?4, finished_at = ?5 WHERE operation_id = ?1",
        params![op, if finished { "succeeded" } else { "running" }, new_done, Value::Object(summary.clone()).to_string(), finished.then_some(now)],
    )
    .await?;
    if !finished {
        return Ok(Some(json!({"kind": "recalculation", "operation_id": op})));
    }
    webhooks::emit(
        h,
        "operation.completed",
        json!({"operation_id": op, "kind": "recalculation", "status": "succeeded", "started_at": rfc3339(r.i("started_at")),
               "finished_at": rfc3339(now), "summary": {"jobs": new_done, "changed": summary},
               "requested_by": {"discord_user_id": r.s("requested_by")}}),
        0,
    )
    .await?;
    Ok(None)
}

/// Every five minutes: forget expired idempotency keys; delete images no
/// submission used.
pub async fn cron<H: Host>(h: &H) -> ApiResult<()> {
    h.run("DELETE FROM idempotency WHERE created_at < ?1", params![h.now_ms() - store::IDEMPOTENCY_TTL_MS]).await?;
    wake_scorers_after_format_change(h).await?;
    crate::manual::sweep(h).await
}

/// After a deploy that changes the scorer's stored format, every scorer is
/// woken once to rebuild, instead of each waiting for its player's next upload
/// (2026-10-20: a scorer stuck on the old format held a delivered job back).
/// The marker is an `operations` row per format.
pub async fn wake_scorers_after_format_change<H: Host>(h: &H) -> ApiResult<()> {
    let marker = format!("op_scorer_format_{}", crate::scorer::SNAPSHOT_FORMAT);
    let added = h
        .run(
            "INSERT OR IGNORE INTO operations (operation_id, kind, status, total, done, requested_by, reason, started_at, finished_at)
             VALUES (?1, 'scorer_format', 'succeeded', 0, 0, 'system', 'Scorer format changed: every scorer rebuilds', ?2, ?2)",
            params![&marker, h.now_ms()],
        )
        .await?;
    if added > 0 {
        h.queue_op(json!({"kind": "poke_all"})).await?;
    }
    Ok(())
}

// ---- bonuses (§9) ----------------------------------------------------------------

fn bonus_status(r: &Row, now: i64) -> &'static str {
    if r.opt_i("archived_at").is_some() {
        "archived"
    } else if now < r.i("active_from") {
        "scheduled"
    } else if now >= r.i("active_to") {
        "ended"
    } else {
        "active"
    }
}

fn bonus_json(r: &Row, now: i64) -> Value {
    json!({"bonus_id": r.s("bonus_id"), "game": r.s("game"), "label": r.s("label"), "match": r.s("match"),
           "match_kind": r.s("match_kind"), "kind": r.s("kind"), "amount": r.f("amount"),
           "active_from": rfc3339(r.i("active_from")), "active_to": rfc3339(r.i("active_to")),
           "status": bonus_status(r, now), "created_at": rfc3339(r.i("created_at")), "updated_at": rfc3339(r.i("updated_at"))})
}

/// Validated bonus fields, starting from `base` (an existing bonus) for PATCH.
fn bonus_fields(body: &Value, base: Option<&Value>) -> ApiResult<Value> {
    let get = |k: &str| body.get(k).filter(|v| !v.is_null()).or_else(|| base.and_then(|b| b.get(k)));
    let mut errs = serde_json::Map::new();
    let s = |k: &str| get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let game = s("game");
    if pt_core::Game::from_api(&game).is_none() {
        errs.insert("game".into(), "ets2 or ats".into());
    }
    let label = s("label");
    if !(1..=100).contains(&label.chars().count()) {
        errs.insert("label".into(), "1-100 characters".into());
    }
    let m = s("match");
    if !(1..=100).contains(&m.len()) {
        errs.insert("match".into(), "1-100 characters".into());
    }
    let match_kind = s("match_kind");
    if !matches!(match_kind.as_str(), "exact" | "glob") {
        errs.insert("match_kind".into(), "exact or glob".into());
    }
    let kind = s("kind");
    let amount = get("amount").and_then(Value::as_f64).unwrap_or(f64::NAN);
    match kind.as_str() {
        "flat" if amount.is_finite() && amount > 0.0 => {}
        "multiplier" if amount.is_finite() && amount > 1.0 => {}
        "flat" | "multiplier" => {
            errs.insert("amount".into(), "flat: > 0; multiplier: > 1".into());
        }
        _ => {
            errs.insert("kind".into(), "flat or multiplier".into());
        }
    }
    let from = util::parse_rfc3339(&s("active_from"));
    let to = util::parse_rfc3339(&s("active_to"));
    match (from, to) {
        (Some(a), Some(b)) if a < b => {}
        (Some(_), Some(_)) => {
            errs.insert("active_to".into(), "must be after active_from".into());
        }
        _ => {
            errs.insert("active_from".into(), "RFC 3339 instants required".into());
        }
    }
    if !errs.is_empty() {
        return Err(ApiError::validation(Value::Object(errs)));
    }
    Ok(json!({"game": game, "label": label, "match": m, "match_kind": match_kind, "kind": kind, "amount": amount,
              "active_from": from, "active_to": to}))
}

pub async fn bonuses_list<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let rows = h.all("SELECT * FROM bonuses ORDER BY active_from DESC, bonus_id", vec![]).await?;
    let game = req.q("game");
    let status = req.q("status");
    let all: Vec<Value> = rows
        .iter()
        .filter(|r| game.is_none_or(|g| r.s("game") == g))
        .filter(|r| match status {
            Some(s) => bonus_status(r, now) == s,
            None => bonus_status(r, now) != "archived",
        })
        .map(|r| bonus_json(r, now))
        .collect();
    let total = all.len();
    let items = all.into_iter().skip(offset).take(limit).collect();
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, now)))
}

async fn bonus_row<H: Host>(h: &H, id: &str) -> ApiResult<Row> {
    store::one(h, "SELECT * FROM bonuses WHERE bonus_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("bonus"))
}

pub async fn bonus_get<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    Ok(Response::ok(bonus_json(&bonus_row(h, id).await?, h.now_ms())))
}

pub async fn bonus_create<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let f = bonus_fields(&body, None)?;
    let id = store::new_id(h, "bon");
    let now = h.now_ms();
    h.batch(vec![
        (
            "INSERT INTO bonuses (bonus_id, game, label, match, match_kind, kind, amount, active_from, active_to, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)"
                .into(),
            params![&id, f["game"].as_str(), f["label"].as_str(), f["match"].as_str(), f["match_kind"].as_str(), f["kind"].as_str(),
                    f["amount"].as_f64(), f["active_from"].as_i64(), f["active_to"].as_i64(), now],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "bonus.create", target_kind: "bonus", target_id: &id,
            target_user: None, reason: Some(&reason), before: json!({}), after: f.clone(),
        }),
    ])
    .await?;
    done(h, idem, 201, bonus_json(&bonus_row(h, &id).await?, now)).await
}

pub async fn bonus_update<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let row = bonus_row(h, id).await?;
    if row.opt_i("archived_at").is_some() {
        return Err(ApiError::invalid_transition("archived", "update", "An archived bonus cannot be changed."));
    }
    let now = h.now_ms();
    let before = bonus_json(&row, now);
    let f = bonus_fields(&body, Some(&before))?;
    h.batch(vec![
        (
            "UPDATE bonuses SET game = ?2, label = ?3, match = ?4, match_kind = ?5, kind = ?6, amount = ?7,
                 active_from = ?8, active_to = ?9, updated_at = ?10 WHERE bonus_id = ?1"
                .into(),
            params![id, f["game"].as_str(), f["label"].as_str(), f["match"].as_str(), f["match_kind"].as_str(), f["kind"].as_str(),
                    f["amount"].as_f64(), f["active_from"].as_i64(), f["active_to"].as_i64(), now],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "bonus.update", target_kind: "bonus", target_id: id,
            target_user: None, reason: Some(&reason), before, after: f,
        }),
    ])
    .await?;
    done(h, idem, 200, bonus_json(&bonus_row(h, id).await?, now)).await
}

pub async fn bonus_archive<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let (admin, idem, _, reason) = begin!(h, req);
    let row = bonus_row(h, id).await?;
    let now = h.now_ms();
    h.batch(vec![
        ("UPDATE bonuses SET archived_at = COALESCE(archived_at, ?2), updated_at = ?2 WHERE bonus_id = ?1".into(), params![id, now]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "bonus.archive", target_kind: "bonus", target_id: id,
            target_user: None, reason: Some(&reason), before: bonus_json(&row, now), after: json!({"status": "archived"}),
        }),
    ])
    .await?;
    done(h, idem, 200, bonus_json(&bonus_row(h, id).await?, now)).await
}

// ---- players (§10) ---------------------------------------------------------------

async fn player_or_404<H: Host>(h: &H, discord_id: &str) -> ApiResult<User> {
    store::check_discord_id(discord_id)?;
    store::user_by_discord(h, discord_id).await?.ok_or_else(|| ApiError::not_found("player"))
}

pub async fn player<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let user = player_or_404(h, discord_id).await?;
    let mut v = public::profile(h, &user).await?;
    v["role"] = user.role.clone().into();
    v["created_at"] = rfc3339(user.created_at).into();
    v["token"] = store::active_token(h, &user.user_id).await?.unwrap_or(Value::Null);
    v["truckersmp_flags"] = admin::player_flags(h, Some(&user)).await?.into();
    Ok(Response::ok(v))
}

pub async fn player_issue_token<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    store::check_discord_id(discord_id)?;
    let name = body.get("display_name").and_then(Value::as_str);
    let (user, created) = store::ensure_user(h, discord_id, name).await?;
    if user.disabled {
        return Err(ApiError::forbidden("This player is disabled."));
    }
    let (token, previous) = store::issue_token(h, &user).await?;
    let mut user_json = user.brief();
    user_json["created"] = created.into();
    let resp = json!({"user": user_json, "token": token});
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "token.issue", target_kind: "user", target_id: &user.user_id,
        target_user: Some(&user.discord_user_id), reason: Some(&reason),
        before: json!({"token_id": previous}), after: json!({"token_id": token["token_id"]}),
    })
    .await?;
    store::idem_store(h, idem, 200, &public::replayed_token(&resp)).await?;
    Ok(Response::json(201, &resp))
}

pub async fn player_revoke_token<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    let (admin, idem, _, reason) = begin!(h, req);
    let user = player_or_404(h, discord_id).await?;
    let id = store::revoke_token(h, &user).await?;
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "token.revoke", target_kind: "user", target_id: &user.user_id,
        target_user: Some(&user.discord_user_id), reason: Some(&reason), before: json!({"token_id": id}), after: json!({}),
    })
    .await?;
    done(h, idem, 200, json!({"revoked_token_id": id})).await
}

pub async fn player_update<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let user = player_or_404(h, discord_id).await?;
    let now = h.now_ms();
    let flag = |k: &str| -> ApiResult<Option<bool>> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(ApiError::field(k, "must be true or false")),
        }
    };
    let division = flag("singleplayer_division")?.unwrap_or(user.singleplayer_division);
    let disabled = flag("disabled")?.unwrap_or(user.disabled);
    let name = match body.get("display_name").and_then(Value::as_str) {
        Some(n) => store::check_display_name(n)?,
        None => user.display_name.clone(),
    };
    let changed_at = if division != user.singleplayer_division { now } else { user.division_changed_at };
    let before = json!({"singleplayer_division": user.singleplayer_division, "disabled": user.disabled, "display_name": user.display_name});
    let after = json!({"singleplayer_division": division, "disabled": disabled, "display_name": name});
    h.batch(vec![
        (
            "UPDATE users SET singleplayer_division = ?2, division_changed_at = ?3, disabled = ?4, display_name = ?5 WHERE user_id = ?1".into(),
            params![&user.user_id, division, changed_at, disabled, &name],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "player.update", target_kind: "user", target_id: &user.user_id,
            target_user: Some(&user.discord_user_id), reason: Some(&reason), before, after,
        }),
    ])
    .await?;
    let user = player_or_404(h, discord_id).await?;
    let mut v = public::profile(h, &user).await?;
    v["role"] = user.role.clone().into();
    done(h, idem, 200, v).await
}

// ---- config versions, releases, revocations (§11) ----------------------------------

pub const MAX_TOML_BYTES: usize = 256 << 10;
pub const MAX_RELEASE_BYTES: usize = 1 << 20;
pub const REVOCATION_NOTICE_MS: i64 = 21 * 86_400_000;

fn config_json(r: &Row) -> Value {
    json!({"version_id": r.s("version_id"), "integrity_hash": r.s("integrity_hash"), "current": r.b("current"),
           "created_at": rfc3339(r.i("created_at"))})
}

pub async fn config_create<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    if req.body.len() > MAX_TOML_BYTES + 4096 {
        return Err(ApiError::too_large(MAX_TOML_BYTES));
    }
    let (admin, idem, body, reason) = begin!(h, req);
    let text = body.get("toml").and_then(Value::as_str).unwrap_or_default();
    if text.len() > MAX_TOML_BYTES {
        return Err(ApiError::too_large(MAX_TOML_BYTES));
    }
    let cfg = Config::from_toml_str(text).and_then(|c| c.validate().map(|_| c)).map_err(|e| {
        ApiError::new(422, "validation_failed", "The config is not valid.").details(json!({"errors": [e.to_string()]}))
    })?;
    let hash = cfg.integrity_hash();
    if let Some(r) = store::one(h, "SELECT version_id FROM config_versions WHERE integrity_hash = ?1", params![&hash]).await? {
        return Err(ApiError::conflict("This config is already published.", json!({"version_id": r.s("version_id")})));
    }
    let no = store::one(h, "SELECT COALESCE(MAX(version_no), 0) + 1 AS n FROM config_versions", vec![]).await?.map_or(1, |r| r.i("n"));
    let id = format!("cfg_{no}");
    let make_current = body.get("make_current").and_then(Value::as_bool).unwrap_or(false);
    let old_key = config::timeline_key(&config::scoring(h).await?.cfg);
    let mut stmts = Vec::new();
    if make_current {
        stmts.push(("UPDATE config_versions SET current = 0".to_string(), vec![]));
    }
    stmts.push((
        "INSERT INTO config_versions (version_id, version_no, toml, integrity_hash, timeline_hash, current, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            .into(),
        params![&id, no, text, &hash, config::timeline_key(&cfg), make_current, h.now_ms(), &admin.discord_user_id],
    ));
    stmts.push(store::audit_stmt(h, Audit {
        actor: &admin.discord_user_id, action: "config.publish", target_kind: "config_version", target_id: &id,
        target_user: None, reason: Some(&reason), before: json!({}), after: json!({"integrity_hash": hash, "current": make_current}),
    }));
    h.batch(stmts).await?;
    if make_current && config::timeline_key(&cfg) != old_key {
        h.queue_op(json!({"kind": "poke_all"})).await?;
    }
    let row = store::one(h, "SELECT * FROM config_versions WHERE version_id = ?1", params![&id]).await?.expect("just inserted");
    done(h, idem, 201, config_json(&row)).await
}

pub async fn config_list<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let rows = h.all("SELECT * FROM config_versions ORDER BY version_no DESC", vec![]).await?;
    let total = rows.len();
    let items = rows.iter().skip(offset).take(limit).map(config_json).collect();
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, now)))
}

pub async fn config_make_current<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let (admin, idem, _, reason) = begin!(h, req);
    let row = store::one(h, "SELECT * FROM config_versions WHERE version_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("config_version"))?;
    let old_key = config::timeline_key(&config::scoring(h).await?.cfg);
    h.batch(vec![
        ("UPDATE config_versions SET current = (version_id = ?1)".into(), params![id]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "config.make_current", target_kind: "config_version", target_id: id,
            target_user: None, reason: Some(&reason), before: json!({}), after: json!({"current": true}),
        }),
    ])
    .await?;
    if row.s("timeline_hash") != old_key {
        h.queue_op(json!({"kind": "poke_all"})).await?;
    }
    let row = store::one(h, "SELECT * FROM config_versions WHERE version_id = ?1", params![id]).await?.expect("exists");
    done(h, idem, 200, config_json(&row)).await
}

pub fn release_json(r: &Row) -> Value {
    let mut v = json!({"release_id": r.s("release_id"), "release_version": r.s("release_version"), "platform": r.s("platform"),
           "files": r.json("files"), "created_at": rfc3339(r.i("created_at"))});
    if let (Value::Object(o), Value::Object(extra)) = (&mut v, crate::updates::release_extras(r)) {
        o.extend(extra);
    }
    v
}

pub async fn release_create<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    if req.body.len() > MAX_RELEASE_BYTES {
        return Err(ApiError::too_large(MAX_RELEASE_BYTES));
    }
    let (admin, idem, body, reason) = begin!(h, req);
    let version = body.get("release_version").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let platform = body.get("platform").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut errs = serde_json::Map::new();
    if version.is_empty() || version.len() > 32 || !version.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        errs.insert("release_version".into(), "like 0.1.0".into());
    }
    if !matches!(platform.as_str(), "windows" | "linux") {
        errs.insert("platform".into(), "windows or linux".into());
    }
    let files = body.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    let ok_file = |f: &Value| {
        matches!(f["role"].as_str(), Some("plugin" | "client_binary" | "config_file"))
            && f["path_pattern"].as_str().is_some_and(|p| !p.is_empty())
            && f["sha256"].as_str().is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if files.is_empty() || !files.iter().all(ok_file) {
        errs.insert("files".into(), "[{role: plugin|client_binary|config_file, path_pattern, sha256 (64 hex)}]".into());
    }
    if !errs.is_empty() {
        return Err(ApiError::validation(Value::Object(errs)));
    }
    let files: Vec<Value> = files
        .iter()
        .map(|f| json!({"role": f["role"], "path_pattern": f["path_pattern"], "sha256": f["sha256"].as_str().unwrap_or_default().to_ascii_lowercase()}))
        .collect();
    let id = format!("rel_{}_{platform}", version.replace(['.', '-'], "_"));
    if store::one(h, "SELECT 1 AS x FROM releases WHERE release_id = ?1", params![&id]).await?.is_some() {
        return Err(ApiError::conflict("This release is already published.", json!({"release_id": id})));
    }
    let up = crate::updates::check(h, &body, &version, &platform, &files).await?;
    h.batch(vec![
        (
            "INSERT INTO releases (release_id, release_version, platform, files, created_at, created_by,
                                   update_manifest, update_signature, installer, notes, offered)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
                .into(),
            params![&id, &version, &platform, Value::Array(files.clone()).to_string(), h.now_ms(), &admin.discord_user_id,
                    up.manifest.clone(), up.signature, up.installer.clone(), up.notes, up.offered],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "release.publish", target_kind: "release", target_id: &id,
            target_user: None, reason: Some(&reason), before: json!({}),
            after: json!({"files": files.len(), "update": up.manifest.is_some(), "installer": up.installer, "offered": up.offered}),
        }),
    ])
    .await?;
    let row = store::one(h, "SELECT * FROM releases WHERE release_id = ?1", params![&id]).await?.expect("just inserted");
    done(h, idem, 201, release_json(&row)).await
}

pub async fn release_list<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let rows = h.all("SELECT * FROM releases ORDER BY created_at DESC", vec![]).await?;
    let total = rows.len();
    let items = rows.iter().skip(offset).take(limit).map(release_json).collect();
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, now)))
}

/// `POST /api/v1/admin/releases/{id}/offer` with `{"offered": bool, "reason"}`:
/// starts or stops offering a release's update to clients (docs/UPDATES.md).
pub async fn release_offer<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let Some(offered) = body.get("offered").and_then(Value::as_bool) else {
        return Err(ApiError::field("offered", "true or false"));
    };
    let row = store::one(h, "SELECT * FROM releases WHERE release_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("release"))?;
    if offered && row.opt_s("update_manifest").is_none() {
        return Err(ApiError::field("offered", "this release has no update"));
    }
    h.batch(vec![
        ("UPDATE releases SET offered = ?2 WHERE release_id = ?1".into(), params![id, offered]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "release.offer", target_kind: "release", target_id: id,
            target_user: None, reason: Some(&reason), before: json!({"offered": row.b("offered")}), after: json!({"offered": offered}),
        }),
    ])
    .await?;
    let row = store::one(h, "SELECT * FROM releases WHERE release_id = ?1", params![id]).await?.expect("exists");
    done(h, idem, 200, release_json(&row)).await
}

/// Discord IDs of players whose latest session used the target.
async fn revocation_recipients<H: Host>(h: &H, kind: &str, target: &Row) -> ApiResult<Vec<String>> {
    let latest = h
        .all(
            "SELECT u.discord_user_id, s.config_hash, s.manifest FROM plugin_sessions s JOIN users u ON u.user_id = s.user_id
             WHERE s.received_at = (SELECT MAX(received_at) FROM plugin_sessions x WHERE x.user_id = s.user_id)",
            vec![],
        )
        .await?;
    let files: Vec<Value> = serde_json::from_str(&target.s("files")).unwrap_or_default();
    let mut out: Vec<String> = latest
        .iter()
        .filter(|s| match kind {
            "config_version" => s.s("config_hash") == target.s("integrity_hash"),
            _ => {
                let entries: Vec<Value> = serde_json::from_str(&s.s("manifest")).unwrap_or_default();
                entries.iter().any(|e| files.iter().any(|f| f["role"] == e["role"] && f["sha256"].as_str().is_some_and(|x| e["sha256"].as_str().is_some_and(|y| x.eq_ignore_ascii_case(y)))))
            }
        })
        .map(|s| s.s("discord_user_id"))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

fn revocation_json(r: &Row, now: i64) -> Value {
    let status = if r.opt_i("cancelled_at").is_some() {
        "cancelled"
    } else if now >= r.i("effective_at") {
        "effective"
    } else {
        "scheduled"
    };
    json!({"revocation_id": r.s("revocation_id"), "target": {"kind": r.s("target_kind"), "id": r.s("target_id")},
           "announced_at": rfc3339(r.i("announced_at")), "effective_at": rfc3339(r.i("effective_at")),
           "status": status, "message": r.s("message")})
}

async fn emit_batches<H: Host>(h: &H, kind: &str, base: Value, recipients: &[String]) -> ApiResult<()> {
    let parts = recipients.len().div_ceil(1000).max(1);
    for (i, batch) in recipients.chunks(1000).enumerate() {
        let mut d = base.clone();
        d["recipients"] = json!(batch);
        d["part"] = json!(i + 1);
        d["parts"] = json!(parts);
        webhooks::emit(h, kind, d, 0).await?;
    }
    Ok(())
}

pub async fn revocation_create<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let kind = body["target"]["kind"].as_str().unwrap_or_default().to_string();
    let id = body["target"]["id"].as_str().unwrap_or_default().to_string();
    let (target, label) = match kind.as_str() {
        "release" => {
            let r = store::one(h, "SELECT * FROM releases WHERE release_id = ?1", params![&id]).await?.ok_or_else(|| ApiError::not_found("release"))?;
            let platform = r.s("platform");
            let label = format!("PlasmaTrack {} ({}{})", r.s("release_version"), platform[..1].to_uppercase(), &platform[1..]);
            (r, label)
        }
        "config_version" => {
            let r = store::one(h, "SELECT * FROM config_versions WHERE version_id = ?1", params![&id]).await?.ok_or_else(|| ApiError::not_found("config_version"))?;
            (r, format!("PlasmaTrack config {id}"))
        }
        _ => return Err(ApiError::field("target", "kind is release or config_version, with its id")),
    };
    let now = h.now_ms();
    let effective = now + REVOCATION_NOTICE_MS;
    let date = rfc3339(effective);
    let (y, m, d) = util::civil_from_days(effective.div_euclid(86_400_000));
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let message = format!(
        "{label} stops counting jobs on {d} {} {y} at {} UTC. Get the new PlasmaTrack Setup with /download in Discord.",
        MONTHS[m as usize - 1],
        &date[11..16]
    );
    let rid = store::new_id(h, "rev");
    let recipients = revocation_recipients(h, &kind, &target).await?;
    h.batch(vec![
        (
            "INSERT INTO revocations (revocation_id, target_kind, target_id, announced_at, effective_at, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6)".into(),
            params![&rid, &kind, &id, now, effective, &message],
        ),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "revocation.schedule", target_kind: &kind, target_id: &id,
            target_user: None, reason: Some(&reason), before: json!({}), after: json!({"revocation_id": rid, "effective_at": date}),
        }),
    ])
    .await?;
    let base = json!({"revocation_id": rid, "target": {"kind": kind, "id": id, "label": label},
                      "announced_at": rfc3339(now), "effective_at": date, "message": message});
    emit_batches(h, "version.revocation_scheduled", base, &recipients).await?;
    done(h, idem, 201, json!({"revocation_id": rid, "announced_at": rfc3339(now), "effective_at": date, "recipients_count": recipients.len()})).await
}

pub async fn revocation_list<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let now = h.now_ms();
    let (limit, offset) = http::page(req, now)?;
    let rows = h.all("SELECT * FROM revocations ORDER BY announced_at DESC", vec![]).await?;
    let all: Vec<Value> = rows.iter().map(|r| revocation_json(r, now)).filter(|v| req.q("status").is_none_or(|s| v["status"] == s)).collect();
    let total = all.len();
    let items = all.into_iter().skip(offset).take(limit).collect();
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, now)))
}

pub async fn revocation_cancel<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let (admin, idem, _, reason) = begin!(h, req);
    let row = store::one(h, "SELECT * FROM revocations WHERE revocation_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("revocation"))?;
    let now = h.now_ms();
    let status = revocation_json(&row, now)["status"].as_str().unwrap_or_default().to_string();
    if status != "scheduled" {
        return Err(ApiError::invalid_transition(&status, "cancel", "Only a scheduled revocation can be cancelled."));
    }
    let target = match row.s("target_kind").as_str() {
        "release" => store::one(h, "SELECT * FROM releases WHERE release_id = ?1", params![row.s("target_id")]).await?,
        _ => store::one(h, "SELECT * FROM config_versions WHERE version_id = ?1", params![row.s("target_id")]).await?,
    };
    let recipients = match target {
        Some(t) => revocation_recipients(h, &row.s("target_kind"), &t).await?,
        None => Vec::new(),
    };
    h.batch(vec![
        ("UPDATE revocations SET cancelled_at = ?2 WHERE revocation_id = ?1".into(), params![id, now]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "revocation.cancel", target_kind: "revocation", target_id: id,
            target_user: None, reason: Some(&reason), before: json!({"status": "scheduled"}), after: json!({"status": "cancelled"}),
        }),
    ])
    .await?;
    let base = json!({"revocation_id": id, "target": {"kind": row.s("target_kind"), "id": row.s("target_id")}});
    emit_batches(h, "version.revocation_cancelled", base, &recipients).await?;
    let row = store::one(h, "SELECT * FROM revocations WHERE revocation_id = ?1", params![id]).await?.expect("exists");
    done(h, idem, 200, revocation_json(&row, now)).await
}

// ---- admins (§12) ----------------------------------------------------------------

pub async fn admins_list<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    let rows = h.all("SELECT * FROM users WHERE role = 'admin' AND disabled = 0 ORDER BY created_at", vec![]).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let bootstrap = h.settings().bootstrap_admins.contains(&r.s("discord_user_id"));
            json!({"user_id": r.s("user_id"), "discord_user_id": r.s("discord_user_id"), "display_name": r.s("display_name"), "bootstrap": bootstrap})
        })
        .collect();
    Ok(Response::ok(json!({"items": items, "next_cursor": null, "total": items.len()})))
}

pub async fn admin_add<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let (admin, idem, body, reason) = begin!(h, req);
    let id = body.get("discord_user_id").and_then(Value::as_str).unwrap_or_default().to_string();
    store::check_discord_id(&id)?;
    let (user, _) = store::ensure_user(h, &id, None).await?;
    h.batch(vec![
        ("UPDATE users SET role = 'admin' WHERE user_id = ?1".into(), params![&user.user_id]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "admin.add", target_kind: "user", target_id: &user.user_id,
            target_user: Some(&id), reason: Some(&reason), before: json!({"role": user.role}), after: json!({"role": "admin"}),
        }),
    ])
    .await?;
    done(h, idem, 201, json!({"user_id": user.user_id, "discord_user_id": id, "display_name": user.display_name})).await
}

pub async fn admin_remove<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    let (admin, idem, _, reason) = begin!(h, req);
    let user = player_or_404(h, discord_id).await?;
    if user.role != "admin" {
        return Err(ApiError::not_found("admin"));
    }
    let admins = store::count(h, "SELECT COUNT(*) AS n FROM users WHERE role = 'admin' AND disabled = 0", vec![]).await?;
    if admins <= 1 {
        return Err(ApiError::invalid_transition("admin", "remove", "The last admin cannot be removed."));
    }
    h.batch(vec![
        ("UPDATE users SET role = 'player' WHERE user_id = ?1".into(), params![&user.user_id]),
        store::audit_stmt(h, Audit {
            actor: &admin.discord_user_id, action: "admin.remove", target_kind: "user", target_id: &user.user_id,
            target_user: Some(discord_id), reason: Some(&reason), before: json!({"role": "admin"}), after: json!({"role": "player"}),
        }),
    ])
    .await?;
    done(h, idem, 200, json!({"user_id": user.user_id, "discord_user_id": discord_id, "role": "player"})).await
}
