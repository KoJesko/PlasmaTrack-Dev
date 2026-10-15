//! Manual job submissions through the bot (PUBLIC_API §6): evidence images,
//! the submission itself, its scoring, and who may see the images.
//!
//! A manual job always starts `pending_review` with reason
//! `manual_submission`. Its distance is the game distance ÷ map scale
//! ([`crate::sheet::world_km`]); bonuses apply only when a moderator sets one.

use pt_core::{Config, Game};
use serde_json::{json, Map, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::jobs::{self, Moderator, Trigger};
use crate::public::job_row;
use crate::store::{self, Audit, User};
use crate::util::{self, mi, rfc3339, round2};
use crate::{auth, config, params, sheet, webhooks};

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_MAIN_EVIDENCE: usize = 5;
pub const MAX_IMAGES: usize = 8;
/// Images not used by a submission within a day are deleted.
pub const UNATTACHED_TTL_MS: i64 = 24 * 3_600_000;
pub const MAX_UNATTACHED: usize = 30;
pub const EVIDENCE_URL_TTL_MS: i64 = 60 * 60_000;

/// AVIF (an ISO-BMFF `ftyp` box naming `avif` or `avis` as its major or a
/// compatible brand), WebP, PNG or JPEG, by their first bytes.
pub fn image_type(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 16 && &b[4..8] == b"ftyp" {
        let size = (u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize).clamp(16, b.len().min(256));
        let mut brands = std::iter::once(&b[8..12]).chain(b[16..size].chunks_exact(4));
        return brands.any(|x| x == b"avif" || x == b"avis").then_some("image/avif");
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// The player a bot call acts for (created when new, like `/link`).
async fn player<H: Host>(h: &H, discord_id: &str) -> ApiResult<User> {
    store::check_discord_id(discord_id)?;
    let (user, _) = store::ensure_user(h, discord_id, None).await?;
    if user.disabled {
        return Err(ApiError::forbidden("This player is disabled."));
    }
    Ok(user)
}

// ---- evidence ------------------------------------------------------------------

/// `POST /api/v1/players/{discord_id}/evidence`: one image, the raw bytes.
pub async fn upload<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    if req.body.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::too_large(MAX_IMAGE_BYTES));
    }
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let content_type = image_type(&req.body).ok_or_else(|| ApiError::field("body", "an AVIF, WebP, PNG or JPEG image"))?;
    let user = player(h, discord_id).await?;
    let now = h.now_ms();
    let waiting = store::count(
        h,
        "SELECT COUNT(*) AS n FROM evidence WHERE user_id = ?1 AND job_id IS NULL AND created_at > ?2",
        params![&user.user_id, now - UNATTACHED_TTL_MS],
    )
    .await?;
    if waiting >= MAX_UNATTACHED {
        return Err(ApiError::rate_limited(3600));
    }
    let id = store::new_id(h, "evd");
    let key = format!("evidence/{}/{id}", user.user_id);
    let sha = util::sha256_hex(&req.body);
    h.blob_put(&key, req.body.clone()).await?;
    h.run(
        "INSERT INTO evidence (evidence_id, user_id, content_type, size_bytes, sha256, r2_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![&id, &user.user_id, content_type, req.body.len() as i64, &sha, &key, now],
    )
    .await?;
    let resp = json!({"evidence_id": id, "content_type": content_type, "size_bytes": req.body.len(), "sha256": sha,
                      "expires_at": rfc3339(now + UNATTACHED_TTL_MS)});
    store::idem_store(h, idem, 201, &resp).await?;
    Ok(Response::json(201, &resp))
}

fn evidence_sig<H: Host>(h: &H, id: &str, exp_s: i64) -> String {
    util::hex(&util::hmac_sha256(h.settings().download_secret.as_bytes(), format!("evidence.{id}.{exp_s}").as_bytes()))
}

/// A signed, time-limited download link for one image.
pub fn evidence_link<H: Host>(h: &H, id: &str) -> Value {
    let exp_s = (h.now_ms() + EVIDENCE_URL_TTL_MS) / 1000;
    let url = format!("{}/dl/evidence/{id}?exp={exp_s}&sig={}", h.settings().public_url.trim_end_matches('/'), evidence_sig(h, id, exp_s));
    json!({"url": url, "expires_at": rfc3339(exp_s * 1000)})
}

/// The images of a job, each with a fresh link.
pub async fn evidence_list<H: Host>(h: &H, job_id: &str) -> ApiResult<Vec<Value>> {
    let rows = h.all("SELECT * FROM evidence WHERE job_id = ?1 ORDER BY role, created_at, evidence_id", params![job_id]).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let mut v = json!({"evidence_id": r.s("evidence_id"), "role": r.s("role"), "content_type": r.s("content_type"),
                               "size_bytes": r.i("size_bytes")});
            let link = evidence_link(h, &r.s("evidence_id"));
            v["url"] = link["url"].clone();
            v["expires_at"] = link["expires_at"].clone();
            v
        })
        .collect())
}

/// `GET /api/v1/players/{discord_id}/jobs/{job_id}/evidence`: the submitter's own images.
pub async fn player_evidence<H: Host>(h: &H, req: &Request, discord_id: &str, raw_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    store::check_discord_id(discord_id)?;
    let user = store::user_by_discord(h, discord_id).await?.ok_or_else(|| ApiError::not_found("player"))?;
    let row = job_row(h, raw_id).await?;
    if row.s("user_id") != user.user_id {
        return Err(ApiError::not_found("job"));
    }
    let job_id = row.s("job_id");
    Ok(Response::ok(json!({"job_id": job_id, "items": evidence_list(h, &job_id).await?})))
}

/// `GET /dl/evidence/{evidence_id}?exp=&sig=`: the signed download itself.
pub async fn download<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    let bad = || ApiError::forbidden("This link is not valid.");
    let (Some(exp), Some(sig)) = (req.q("exp"), req.q("sig")) else { return Err(bad()) };
    let exp_s: i64 = exp.parse().map_err(|_| bad())?;
    if !util::constant_eq(sig.as_bytes(), evidence_sig(h, id, exp_s).as_bytes()) {
        return Err(bad());
    }
    if h.now_ms() / 1000 > exp_s {
        return Err(ApiError::forbidden("This link has expired."));
    }
    let row = store::one(h, "SELECT * FROM evidence WHERE evidence_id = ?1", params![id]).await?.ok_or_else(|| ApiError::not_found("evidence"))?;
    let blob = h.blob_get(&row.s("r2_key")).await?.ok_or_else(|| ApiError::not_found("evidence"))?;
    Ok(Response::bytes(200, &row.s("content_type"), blob).with_header("cache-control", "private, max-age=3600"))
}

/// A manual job's custom map: absent for tracked jobs, and from the public
/// view when a moderator hid it.
pub async fn custom_map<H: Host>(h: &H, row: &Row, moderator: bool) -> ApiResult<Value> {
    if row.s("origin") != "manual" || (row.b("hide_map") && !moderator) {
        return Ok(Value::Null);
    }
    let r = store::one(h, "SELECT evidence_id FROM evidence WHERE job_id = ?1 AND role = 'custom_map'", params![row.s("job_id")]).await?;
    Ok(r.map_or(Value::Null, |r| evidence_link(h, &r.s("evidence_id"))))
}

/// Daily housekeeping: images never used by a submission.
pub async fn sweep<H: Host>(h: &H) -> ApiResult<()> {
    let old = h
        .all("SELECT evidence_id, r2_key FROM evidence WHERE job_id IS NULL AND created_at < ?1 LIMIT 100", params![h.now_ms() - UNATTACHED_TTL_MS])
        .await?;
    for r in old {
        h.blob_delete(&r.s("r2_key")).await?;
        h.run("DELETE FROM evidence WHERE evidence_id = ?1", params![r.s("evidence_id")]).await?;
    }
    Ok(())
}

// ---- submission ----------------------------------------------------------------

/// `POST /api/v1/players/{discord_id}/manual-jobs`.
pub async fn submit<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let body = req.json_body()?;
    let game = body.get("game").and_then(Value::as_str).and_then(Game::from_api).ok_or_else(|| ApiError::field("game", "ets2 or ats"))?;
    let user = player(h, discord_id).await?;
    let mut fields = sheet::parse(&body, game, true, &Map::new())?;
    let scoring = config::scoring(h).await?;
    let defaulted = !fields.contains_key("map_scale");
    if defaulted {
        fields.insert("map_scale".into(), json!(scoring.cfg.map.scale.vanilla(game).main_road_local_scale()));
    }
    sheet::check(&fields)?;

    // Every referenced image: the player's own, unused, at most once.
    let mut refs: Vec<(&str, String, String)> = Vec::new();
    let mut errs = Map::new();
    match body.get("evidence").and_then(Value::as_array) {
        Some(a) if (1..=MAX_MAIN_EVIDENCE).contains(&a.len()) => {
            for (i, v) in a.iter().enumerate() {
                match v.as_str() {
                    Some(id) => refs.push(("main", id.to_string(), format!("evidence[{i}]"))),
                    None => {
                        errs.insert(format!("evidence[{i}]"), "an evidence id".into());
                    }
                }
            }
        }
        _ => {
            errs.insert("evidence".into(), format!("1-{MAX_MAIN_EVIDENCE} evidence ids").into());
        }
    }
    match body.get("custom_map") {
        None | Some(Value::Null) => {}
        Some(Value::String(id)) => refs.push(("custom_map", id.clone(), "custom_map".into())),
        Some(_) => {
            errs.insert("custom_map".into(), "an evidence id".into());
        }
    }
    for (sec, role, field) in [("uk", "uk_route", "uk.route_evidence"), ("adjusted_scale", "adjusted_scale", "adjusted_scale.evidence")] {
        if let Some(id) = fields.get(sec).and_then(|s| s.get("evidence_id")).and_then(Value::as_str) {
            refs.push((role, id.to_string(), field.into()));
        }
    }
    if refs.len() > MAX_IMAGES {
        errs.insert("evidence".into(), format!("at most {MAX_IMAGES} images per job").into());
    }
    for (i, (_, id, field)) in refs.iter().enumerate() {
        if refs[..i].iter().any(|(_, x, _)| x == id) {
            errs.insert(field.clone(), "used twice".into());
            continue;
        }
        let ok = store::one(h, "SELECT 1 AS ok FROM evidence WHERE evidence_id = ?1 AND user_id = ?2 AND job_id IS NULL", params![id, &user.user_id])
            .await?
            .is_some();
        if !ok {
            errs.insert(field.clone(), "not an unused image uploaded for this player".into());
        }
    }
    if !errs.is_empty() {
        return Err(ApiError::validation(Value::Object(errs)));
    }

    let job_id = store::new_job_id(h);
    let submission = json!({
        "unit": body.get("unit"), "fuel_unit": body.get("fuel_unit"), "map_scale_defaulted": defaulted,
        "sheet": Value::Object(fields),
    });
    let mut local = [0u8; 4];
    h.random(&mut local);
    let local_id = (u32::from_le_bytes(local) & 0x7fff_ffff) as i64;
    let now = h.now_ms();
    h.run(
        "INSERT INTO jobs (job_id, user_id, game, profile_key, local_id, version, state, start_kind, outcome, auto_outcome,
             summary, detail, updated_at, origin, submission, delivered_at, multiplayer, config_version_id)
         VALUES (?1, ?2, ?3, 'manual', ?4, 0, 'delivered', 'manual', 'new', 'pending_review', '{}', '{}', ?5, 'manual', ?6, ?5,
                 'unknown', ?7)",
        params![&job_id, &user.user_id, game.as_str(), local_id, now, submission.to_string(), scoring.version_id.as_deref()],
    )
    .await?;
    let row = store::one(h, "SELECT * FROM jobs WHERE job_id = ?1", params![&job_id]).await?.ok_or_else(|| ApiError::internal("job vanished"))?;
    if let Err(e) = store_scored(h, &scoring.cfg, &user, &row, Trigger::Upload, None).await {
        h.run("DELETE FROM jobs WHERE job_id = ?1", params![&job_id]).await?;
        return Err(e);
    }
    for (role, id, _) in &refs {
        h.run("UPDATE evidence SET job_id = ?2, role = ?3 WHERE evidence_id = ?1", params![id, &job_id, *role]).await?;
    }
    store::audit(h, Audit {
        actor: "self", action: "job.submit", target_kind: "job", target_id: &job_id, target_user: Some(&user.discord_user_id),
        reason: None, before: json!({}), after: json!({"game": game.as_str(), "evidence": refs.len()}),
    })
    .await?;
    let row = job_row(h, &job_id).await?;
    let mut resp = jobs::detail_json(&row, &user, &scoring.cfg);
    resp["custom_map"] = custom_map(h, &row, false).await?;
    store::idem_store(h, idem, 201, &resp).await?;
    Ok(Response::json(201, &resp))
}

// ---- scoring -------------------------------------------------------------------

/// The submission's sheet with the moderator's overrides over it.
pub fn effective(row: &Row) -> Map<String, Value> {
    let sub = row.json("submission");
    let base: Map<String, Value> = serde_json::from_value(sub["sheet"].clone()).unwrap_or_default();
    let overrides: Map<String, Value> = serde_json::from_value(row.json("overrides")).unwrap_or_default();
    sheet::merged(&base, &overrides)
}

fn place(city: Option<&Value>, company: Option<&Value>) -> Value {
    json!({"city_id": null, "city_name": city.cloned().unwrap_or(Value::Null), "company_id": null,
           "company_name": company.cloned().unwrap_or(Value::Null)})
}

/// Re-scores a manual job with the current config (recalculation, edits).
pub async fn rescore<H: Host>(h: &H, row: &Row, trigger: Trigger, moderator: Option<Moderator<'_>>) -> ApiResult<()> {
    let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("user"))?;
    let cfg = config::scoring(h).await?.cfg;
    store_scored(h, &cfg, &user, row, trigger, moderator).await
}

/// Scores a manual job from its sheet and stores the result; emits the
/// webhooks the change calls for.
async fn store_scored<H: Host>(h: &H, cfg: &Config, user: &User, row: &Row, trigger: Trigger, moderator: Option<Moderator<'_>>) -> ApiResult<()> {
    let game = Game::from_api(&row.s("game")).ok_or_else(|| ApiError::internal("bad game"))?;
    let eff = effective(row);
    let (world, classes) = sheet::world_km(&eff, game, cfg);
    let game_km = eff.get("distance_km").and_then(Value::as_f64).unwrap_or(0.0);
    let bonus_rows = jobs::bonuses(h, game).await?;
    let bonus = eff.get("bonus_id").and_then(Value::as_str).and_then(|id| bonus_rows.iter().find(|(b, _)| b.id == id));
    let world_mi = pt_core::units::km_to_mi(world);
    let base_points = world_mi * cfg.rules.points_per_world_mi;
    let raw = bonus.map_or(base_points, |(b, _)| jobs::apply_bonus(b, base_points));
    let computed = cfg.rules.floor_points(raw);

    let old = row.s("outcome");
    let is_new = old == "new";
    let outcome = if jobs::DECIDED.contains(&old.as_str()) { old.clone() } else { "pending_review".to_string() };
    let (points, held) = jobs::points_for(&outcome, computed, computed);
    let reasons = json!([jobs::reason("manual_submission", json!({}))]);
    let s = |k: &str| eff.get(k);
    let summary = json!({
        "cargo": {"id": null, "name": s("cargo_name")},
        "source": place(s("source_city"), s("source_company")),
        "destination": place(s("destination_city"), s("destination_company")),
        "market": null,
        "world_km": round2(world), "world_mi": mi(world), "game_km": round2(game_km), "game_mi": mi(game_km),
        "planned_distance_km": null,
        "speed_penalty_applied": false,
        "bonus": bonus.map_or(Value::Null, |(b, l)| jobs::bonus_json(b, l)),
        "distance_by_scale_class": jobs::by_class_json(&classes),
        "multiplayer": {"kind": "unknown", "truckersmp": null},
    });
    let detail = json!({
        "state": "delivered", "start_kind": "manual", "profile_key": "manual", "plugin_sessions": [],
        "bonus_set": bonus.map_or(json!([]), |(b, l)| json!([{"bonus_id": b.id, "label": l, "applied": true}])),
        "distances": {"delivered_game_km": round2(game_km), "tracked_whole_game_km": null, "tracked_counted_game_km": null,
                      "tracked_uncounted_game_km": null, "counted_game_km": round2(game_km), "world_km": util::round3(world),
                      "world_mi": util::round3(world_mi), "accepted_world_mi": util::round3(world_mi)},
        "speed": null,
        "fuel": {"used_l": eff.get("fuel_used_l").and_then(Value::as_f64).map(round2), "refuels": []},
        "rules": [], "integrity": null, "segments": [], "timeline": [],
    });
    let changed = is_new
        || row.json("summary") != summary
        || row.json("detail") != detail
        || row.opt_f("points") != points.as_f64()
        || row.opt_f("held_points") != held.as_f64();
    if !changed {
        return Ok(());
    }
    let version = row.i("version") + 1;
    let job_id = row.s("job_id");
    h.run(
        "UPDATE jobs SET version = ?2, outcome = ?3, points = ?4, held_points = ?5, computed_points = ?6, world_km = ?7,
             game_km = ?8, reasons = ?9, bonus_id = ?10, reported_delivered_at = ?11, started_wall_ms = ?12, summary = ?13,
             detail = ?14, updated_at = ?15 WHERE job_id = ?1",
        params![
            &job_id, version, &outcome, points.as_f64(), held.as_f64(), computed, round2(world), round2(game_km), reasons.to_string(),
            bonus.map(|(b, _)| b.id.clone()), eff.get("ended_at_ms").and_then(Value::as_i64),
            eff.get("started_at_ms").and_then(Value::as_i64).unwrap_or(0), summary.to_string(), detail.to_string(), h.now_ms()
        ],
    )
    .await?;
    if moderator.is_some_and(|m| m.quiet) {
        return Ok(());
    }
    let player = user.brief();
    if is_new {
        webhooks::emit(
            h,
            "job.needs_review",
            json!({"job_id": job_id, "game": game.as_str(), "job_version": version, "player": player, "outcome": outcome,
                   "held_points": held, "reasons": reasons, "delivered_at": util::opt_rfc3339(row.opt_i("delivered_at")), "origin": "manual"}),
            0,
        )
        .await?;
    } else if row.opt_f("points") != points.as_f64() || row.opt_f("held_points") != held.as_f64() || old != outcome {
        webhooks::emit(
            h,
            "job.scored",
            json!({"job_id": job_id, "game": game.as_str(), "job_version": version, "player": player, "outcome": outcome,
                   "previous_outcome": old, "points": points, "held_points": held, "by": jobs::by_json(trigger, moderator)}),
            0,
        )
        .await?;
    }
    Ok(())
}

// ---- moderator edits (ADMIN_API §14) --------------------------------------------

/// Validates an edit of any job and stores the new overrides; returns the
/// overrides before and after. The caller re-scores.
pub async fn write_overrides<H: Host>(h: &H, row: &Row, edit: &Value) -> ApiResult<(Value, Value)> {
    let game = Game::from_api(&row.s("game")).ok_or_else(|| ApiError::internal("bad game"))?;
    let before: Map<String, Value> = serde_json::from_value(row.json("overrides")).unwrap_or_default();
    let base: Map<String, Value> = if row.s("origin") == "manual" {
        serde_json::from_value(row.json("submission")["sheet"].clone()).unwrap_or_default()
    } else {
        serde_json::from_value(row.json("detail")["sheet_base"].clone()).unwrap_or_default()
    };
    let current = sheet::merged(&base, &before);
    let patch = sheet::parse(edit, game, false, &current)?;
    if let Some(id) = patch.get("bonus_id").and_then(Value::as_str) {
        let ok = store::one(h, "SELECT 1 AS ok FROM bonuses WHERE bonus_id = ?1 AND game = ?2", params![id, game.as_str()]).await?.is_some();
        if !ok {
            return Err(ApiError::field("bonus_id", "no such bonus for this game"));
        }
    }
    let mut after = before.clone();
    for (k, v) in patch {
        // Back to the recording's or the submission's value: drop the override.
        if base.get(&k) == Some(&v) {
            after.remove(&k);
        } else {
            after.insert(k, v);
        }
    }
    sheet::check(&sheet::merged(&base, &after))?;
    h.run("UPDATE jobs SET overrides = ?2 WHERE job_id = ?1", params![row.s("job_id"), Value::Object(after.clone()).to_string()]).await?;
    Ok((Value::Object(before), Value::Object(after)))
}

/// The admin detail's extra fields: origin, the sheet (effective values), the
/// submission, the overrides, the images and `hide_map`.
pub async fn admin_extras<H: Host>(h: &H, row: &Row, v: &mut Value) -> ApiResult<()> {
    let overrides: Map<String, Value> = serde_json::from_value(row.json("overrides")).unwrap_or_default();
    let manual = row.s("origin") == "manual";
    let base: Map<String, Value> = if manual {
        serde_json::from_value(row.json("submission")["sheet"].clone()).unwrap_or_default()
    } else {
        serde_json::from_value(row.json("detail")["sheet_base"].clone()).unwrap_or_default()
    };
    v["origin"] = row.s("origin").into();
    v["sheet"] = sheet::to_json(&sheet::merged(&base, &overrides));
    v["overrides"] = sheet::to_json(&overrides);
    v["submission"] = if manual {
        let sub = row.json("submission");
        json!({"unit": sub["unit"], "fuel_unit": sub["fuel_unit"], "map_scale_defaulted": sub["map_scale_defaulted"],
               "sheet": sheet::to_json(&base)})
    } else {
        Value::Null
    };
    v["evidence"] = evidence_list(h, &row.s("job_id")).await?.into();
    v["hide_map"] = row.b("hide_map").into();
    if let Some(o) = v.as_object_mut() {
        o.remove("sheet_base");
    }
    Ok(())
}

/// For the public detail and lists: `origin` (and `custom_map` on the detail).
pub async fn public_extras<H: Host>(h: &H, row: &Row, v: &mut Value) -> ApiResult<()> {
    v["origin"] = row.s("origin").into();
    v["custom_map"] = custom_map(h, row, false).await?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::image_type;

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut b = (size as u32).to_be_bytes().to_vec();
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(major);
        b.extend_from_slice(&[0, 0, 0, 0]);
        for c in compatible {
            b.extend_from_slice(*c);
        }
        b.extend_from_slice(b"\0\0\0\x0cmeta");
        b
    }

    #[test]
    fn image_types_by_their_first_bytes() {
        assert_eq!(image_type(&ftyp(b"avif", &[b"mif1", b"miaf"])), Some("image/avif"));
        assert_eq!(image_type(&ftyp(b"mif1", &[b"avif"])), Some("image/avif"));
        assert_eq!(image_type(&ftyp(b"avis", &[])), Some("image/avif"));
        assert_eq!(image_type(&ftyp(b"heic", &[b"mif1"])), None, "HEIC is not AVIF");
        assert_eq!(image_type(&ftyp(b"isom", &[b"mp41"])), None, "a video");
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a"), None);
        assert_eq!(image_type(b"\0\0\0\x08ftyp"), None, "too short");
    }
}
