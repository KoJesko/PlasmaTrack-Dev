//! Player self-service and public reads (docs/PUBLIC_API.md).

use pt_core::Game;
use serde_json::{json, Value};

use crate::env::{Host, RowExt};
use crate::http::{self, ApiError, ApiResult, Request, Response};
use crate::store::{self, Audit, User};
use crate::util::{self, mi, rfc3339, round2};
use crate::{auth, config, jobs, params};

pub fn game(s: &str) -> ApiResult<Game> {
    Game::from_api(s).ok_or_else(|| ApiError::not_found("game"))
}

// ---- self-service (§1) -----------------------------------------------------------

pub async fn issue_token<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    store::check_discord_id(discord_id)?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let body = req.json_body()?;
    let name = body.get("display_name").and_then(Value::as_str);
    if let Some(n) = name {
        store::check_display_name(n)?;
    }
    let (user, created) = store::ensure_user(h, discord_id, name).await?;
    if user.disabled {
        return Err(ApiError::forbidden("This player is disabled."));
    }
    let (token, previous) = store::issue_token(h, &user).await?;
    let mut user_json = user.brief();
    user_json["created"] = created.into();
    let resp = json!({"user": user_json, "token": token});
    store::audit(h, Audit {
        actor: "self", action: "token.issue", target_kind: "user", target_id: &user.user_id,
        target_user: Some(&user.discord_user_id), reason: None,
        before: json!({"token_id": previous}), after: json!({"token_id": token["token_id"]}),
    })
    .await?;
    store::idem_store(h, idem, 200, &replayed_token(&resp)).await?;
    Ok(Response::json(201, &resp))
}

/// A replayed token response: never the plaintext again (PUBLIC_API §1).
pub fn replayed_token(resp: &Value) -> Value {
    let mut r = resp.clone();
    r["token"]["token"] = Value::Null;
    r["details"] = json!({"replayed": true});
    r
}

async fn linked<H: Host>(h: &H, discord_id: &str) -> ApiResult<User> {
    store::check_discord_id(discord_id)?;
    store::user_by_discord(h, discord_id).await?.ok_or_else(|| ApiError::not_found("player"))
}

pub async fn token_status<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    let user = linked(h, discord_id).await?;
    Ok(Response::ok(store::active_token(h, &user.user_id).await?.ok_or_else(|| ApiError::not_found("token"))?))
}

pub async fn revoke_token<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let user = linked(h, discord_id).await?;
    let id = store::revoke_token(h, &user).await?;
    store::audit(h, Audit {
        actor: "self", action: "token.revoke", target_kind: "user", target_id: &user.user_id,
        target_user: Some(&user.discord_user_id), reason: None, before: json!({"token_id": id}), after: json!({}),
    })
    .await?;
    let resp = json!({"revoked_token_id": id});
    store::idem_store(h, idem, 200, &resp).await?;
    Ok(Response::ok(resp))
}

pub async fn rename<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let user = linked(h, discord_id).await?;
    let body = req.json_body()?;
    let name = store::check_display_name(body.get("display_name").and_then(Value::as_str).unwrap_or_default())?;
    h.run("UPDATE users SET display_name = ?2 WHERE user_id = ?1", params![&user.user_id, &name]).await?;
    store::audit(h, Audit {
        actor: "self", action: "player.update", target_kind: "user", target_id: &user.user_id,
        target_user: Some(&user.discord_user_id), reason: None,
        before: json!({"display_name": user.display_name}), after: json!({"display_name": name}),
    })
    .await?;
    let resp = profile(h, &linked(h, discord_id).await?).await?;
    store::idem_store(h, idem, 200, &resp).await?;
    Ok(Response::ok(resp))
}

// ---- profiles and stats (§2) -----------------------------------------------------

pub async fn stats<H: Host>(h: &H, user_id: &str, g: Game) -> ApiResult<Value> {
    let r = store::one(
        h,
        "SELECT COALESCE(SUM(points), 0) AS points, COUNT(*) AS n, COALESCE(SUM(world_km), 0) AS world_km,
                COALESCE(SUM(game_km), 0) AS game_km
         FROM jobs WHERE user_id = ?1 AND game = ?2 AND outcome IN ('valid', 'approved')",
        params![user_id, g.as_str()],
    )
    .await?
    .unwrap_or_default();
    let pending = store::count(
        h,
        "SELECT COUNT(*) AS n FROM jobs WHERE user_id = ?1 AND game = ?2 AND outcome = 'pending_review'",
        params![user_id, g.as_str()],
    )
    .await?;
    let (w, gk) = (r.f("world_km"), r.f("game_km"));
    let cfg = config::scoring(h).await?.cfg;

    // Every outcome, and the counted distance per scale class (PUBLIC_API §2).
    let mut outcomes = serde_json::Map::new();
    for o in OUTCOMES {
        outcomes.insert(o.into(), json!(0));
    }
    for row in h
        .all("SELECT outcome, COUNT(*) AS n FROM jobs WHERE user_id = ?1 AND game = ?2 GROUP BY outcome", params![user_id, g.as_str()])
        .await?
    {
        if OUTCOMES.contains(&row.s("outcome").as_str()) {
            outcomes.insert(row.s("outcome"), json!(row.i("n")));
        }
    }
    const CLASSES: [&str; 5] = ["city", "road", "one_to_one", "nonstandard", "unknown"];
    let sums: Vec<String> = CLASSES
        .iter()
        .flat_map(|c| {
            ["world_km", "game_km"].map(|k| format!("COALESCE(SUM(json_extract(summary, '$.distance_by_scale_class.{c}.{k}')), 0) AS {c}_{k}"))
        })
        .collect();
    let by = store::one(
        h,
        &format!("SELECT {} FROM jobs WHERE user_id = ?1 AND game = ?2 AND outcome IN ('valid', 'approved')", sums.join(", ")),
        params![user_id, g.as_str()],
    )
    .await?
    .unwrap_or_default();
    let mut by_class = serde_json::Map::new();
    for c in CLASSES {
        let (cw, cg) = (by.f(&format!("{c}_world_km")), by.f(&format!("{c}_game_km")));
        by_class.insert(c.into(), json!({"world_km": round2(cw), "world_mi": mi(cw), "game_km": round2(cg), "game_mi": mi(cg)}));
    }
    Ok(json!({
        "points": cfg.rules.floor_points(r.f("points")), "job_count": r.i("n"),
        "world_km": round2(w), "world_mi": mi(w), "game_km": round2(gk), "game_mi": mi(gk),
        "pending_review_count": pending,
        "outcomes": outcomes,
        "distance_by_scale_class": by_class,
    }))
}

pub async fn truckersmp_accounts<H: Host>(h: &H, user_id: &str) -> ApiResult<Vec<Value>> {
    let rows = h
        .all(
            "SELECT a.account_id, a.first_seen_at, a.last_seen_at,
                    (SELECT COUNT(*) FROM jobs j WHERE j.user_id = a.user_id AND j.tmp_account_id = a.account_id
                       AND j.outcome != 'incomplete') AS job_count
             FROM truckersmp_accounts a WHERE a.user_id = ?1 AND a.account_id > 0 ORDER BY a.first_seen_at",
            params![user_id],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({"account_id": r.i("account_id"), "profile_url": format!("https://truckersmp.com/user/{}", r.i("account_id")),
                   "first_seen_at": util::rfc3339(r.i("first_seen_at")), "last_seen_at": util::rfc3339(r.i("last_seen_at")),
                   "job_count": r.i("job_count")})
        })
        .collect())
}

pub async fn profile<H: Host>(h: &H, user: &User) -> ApiResult<Value> {
    Ok(json!({
        "linked": true,
        "user_id": user.user_id,
        "discord_user_id": user.discord_user_id,
        "display_name": user.display_name,
        "singleplayer_division": user.singleplayer_division,
        "disabled": user.disabled,
        "truckersmp_accounts": truckersmp_accounts(h, &user.user_id).await?,
        "stats": {"ets2": stats(h, &user.user_id, Game::Ets2).await?, "ats": stats(h, &user.user_id, Game::Ats).await?},
    }))
}

pub async fn player<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    store::check_discord_id(discord_id)?;
    Ok(Response::ok(match store::user_by_discord(h, discord_id).await? {
        Some(u) => profile(h, &u).await?,
        None => json!({"linked": false, "discord_user_id": discord_id}),
    }))
}

pub async fn player_stats<H: Host>(h: &H, req: &Request, g: &str, discord_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    let g = game(g)?;
    let user = linked(h, discord_id).await?;
    let mut s = stats(h, &user.user_id, g).await?;
    s["game"] = g.as_str().into();
    Ok(Response::ok(s))
}

// ---- jobs (§3) -------------------------------------------------------------------

pub const OUTCOMES: [&str; 7] = ["valid", "approved", "pending_review", "invalid", "rejected", "discarded", "voided"];

pub async fn player_jobs<H: Host>(h: &H, req: &Request, g: &str, discord_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    let g = game(g)?;
    jobs_page(h, req, discord_id, Some(g)).await
}

/// `GET /api/v1/public/players/{discord_id}/jobs`: both games, newest first.
pub async fn player_jobs_all<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    jobs_page(h, req, discord_id, None).await
}

async fn jobs_page<H: Host>(h: &H, req: &Request, discord_id: &str, g: Option<Game>) -> ApiResult<Response> {
    let user = linked(h, discord_id).await?;
    let (limit, offset) = http::page(req, h.now_ms())?;
    let outcomes: Vec<String> = match req.q("outcome") {
        Some(s) if !s.is_empty() => s.split(',').map(|o| o.trim().to_string()).collect(),
        _ => OUTCOMES.iter().map(|o| o.to_string()).collect(),
    };
    if let Some(bad) = outcomes.iter().find(|o| !OUTCOMES.contains(&o.as_str())) {
        return Err(ApiError::bad_request("unknown outcome").details(json!({"outcome": bad})));
    }
    let list = outcomes.iter().map(|o| format!("'{o}'")).collect::<Vec<_>>().join(",");
    let filter = format!("user_id = ?1 AND (?2 IS NULL OR game = ?2) AND outcome IN ({list})");
    let game = g.map(|g| g.as_str());
    let total = store::count(h, &format!("SELECT COUNT(*) AS n FROM jobs WHERE {filter}"), params![&user.user_id, game]).await?;
    let rows = h
        .all(
            &format!("SELECT * FROM jobs WHERE {filter} ORDER BY delivered_at DESC, job_id LIMIT ?3 OFFSET ?4"),
            params![&user.user_id, game, limit as i64, offset as i64],
        )
        .await?;
    let items = rows.iter().map(jobs::summary_json).collect();
    Ok(Response::ok(http::list(items, offset, limit, Some(total), offset + limit < total, h.now_ms())))
}

/// A finished job by id (accepts the forms of API_CONVENTIONS §6).
pub async fn job_row<H: Host>(h: &H, raw_id: &str) -> ApiResult<crate::env::Row> {
    let id = util::normalize_job_id(raw_id).ok_or_else(|| ApiError::not_found("job"))?;
    store::one(h, "SELECT * FROM jobs WHERE job_id = ?1 AND outcome != 'incomplete'", params![id])
        .await?
        .ok_or_else(|| ApiError::not_found("job"))
}

pub async fn job<H: Host>(h: &H, req: &Request, raw_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    let row = job_row(h, raw_id).await?;
    let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("job"))?;
    let cfg = config::scoring(h).await?.cfg;
    let mut v = jobs::detail_json(&row, &user, &cfg);
    crate::manual::public_extras(h, &row, &mut v).await?;
    Ok(Response::ok(v))
}

// ---- history buckets (§7) --------------------------------------------------------

const DAY_MS: i64 = 86_400_000;
pub const MAX_BUCKETS: usize = 400;

fn bucket_start(kind: &str, ms: i64) -> i64 {
    match kind {
        "week" => {
            let (y, w) = util::iso_week_of(ms);
            util::iso_week_start(y, w)
        }
        "month" => {
            let (y, m) = util::year_month_of(ms);
            util::month_start(y, m)
        }
        _ => ms - ms.rem_euclid(DAY_MS),
    }
}

fn bucket_next(kind: &str, start: i64) -> i64 {
    match kind {
        "week" => start + 7 * DAY_MS,
        "month" => bucket_start("month", start + 32 * DAY_MS),
        _ => start + DAY_MS,
    }
}

/// An instant or a date (`2026-10-15`, taken as 00:00 UTC).
fn instant_or_date(name: &str, s: &str) -> ApiResult<i64> {
    util::parse_rfc3339(s)
        .or_else(|| util::parse_rfc3339(&format!("{s}T00:00:00Z")))
        .ok_or_else(|| ApiError::bad_request(&format!("{name} is an RFC 3339 instant or a date")).details(json!({name: s})))
}

/// Sums of counted jobs, as one bucket's numbers.
#[derive(Default, Clone)]
struct Sums {
    jobs: i64,
    points: f64,
    world_km: f64,
    game_km: f64,
    fuel_l: f64,
    income: f64,
    damage_sum: f64,
    damage_n: i64,
}

impl Sums {
    fn add(&mut self, r: &crate::env::Row) {
        self.jobs += 1;
        self.points += r.f("points");
        self.world_km += r.f("world_km");
        self.game_km += r.f("game_km");
        self.fuel_l += r.f("fuel");
        self.income += r.f("income");
        if let Some(d) = r.opt_f("damage") {
            self.damage_sum += d;
            self.damage_n += 1;
        }
    }

    fn merge(&mut self, o: &Sums) {
        self.jobs += o.jobs;
        self.points += o.points;
        self.world_km += o.world_km;
        self.game_km += o.game_km;
        self.fuel_l += o.fuel_l;
        self.income += o.income;
        self.damage_sum += o.damage_sum;
        self.damage_n += o.damage_n;
    }

    fn json(&self, cfg: &pt_core::Config) -> Value {
        json!({"job_count": self.jobs, "points": cfg.rules.floor_points(self.points),
               "world_km": round2(self.world_km), "world_mi": mi(self.world_km),
               "game_km": round2(self.game_km), "game_mi": mi(self.game_km),
               "fuel_used_l": round2(self.fuel_l), "income": self.income.round() as i64,
               "damage_pct_avg": (self.damage_n > 0).then(|| round2(self.damage_sum / self.damage_n as f64))})
    }
}

const COUNTED_COLUMNS: &str = "delivered_at, points, world_km, game_km, json_extract(summary, '$.fuel_used_l') AS fuel,
    json_extract(summary, '$.income') AS income, json_extract(summary, '$.damage_pct') AS damage";

/// `GET /api/v1/public/{game}/players/{discord_id}/history?bucket=day|week|month&from=&to=`
pub async fn player_history<H: Host>(h: &H, req: &Request, g: &str, discord_id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    let g = game(g)?;
    let user = linked(h, discord_id).await?;
    let kind = req.q("bucket").unwrap_or("day");
    if !matches!(kind, "day" | "week" | "month") {
        return Err(ApiError::bad_request("bucket is day, week or month").details(json!({"bucket": kind})));
    }
    let now = h.now_ms();
    let to = match req.q("to") {
        Some(s) => instant_or_date("to", s)?,
        None => now,
    };
    let last = bucket_start(kind, to);
    let first = match req.q("from") {
        Some(s) => bucket_start(kind, instant_or_date("from", s)?),
        None => {
            let back = match kind {
                "week" => 11,
                "month" => 11,
                _ => 29,
            };
            (0..back).fold(last, |b, _| bucket_start(kind, b - 1))
        }
    };
    if first > last {
        return Err(ApiError::bad_request("from is after to"));
    }
    let mut starts = vec![first];
    while *starts.last().expect("one") < last {
        let next = bucket_next(kind, *starts.last().expect("one"));
        starts.push(next);
        if starts.len() > MAX_BUCKETS {
            return Err(ApiError::bad_request(&format!("at most {MAX_BUCKETS} buckets")).details(json!({"max_buckets": MAX_BUCKETS})));
        }
    }
    let end = bucket_next(kind, last);
    let counted = "user_id = ?1 AND game = ?2 AND outcome IN ('valid', 'approved')";
    let mut before = Sums::default();
    for r in h.all(&format!("SELECT {COUNTED_COLUMNS} FROM jobs WHERE {counted} AND delivered_at < ?3"), params![&user.user_id, g.as_str(), first]).await? {
        before.add(&r);
    }
    let rows = h
        .all(
            &format!("SELECT {COUNTED_COLUMNS} FROM jobs WHERE {counted} AND delivered_at >= ?3 AND delivered_at < ?4 ORDER BY delivered_at"),
            params![&user.user_id, g.as_str(), first, end],
        )
        .await?;
    let cfg = config::scoring(h).await?.cfg;
    let mut sums = vec![Sums::default(); starts.len()];
    for r in &rows {
        let t = r.i("delivered_at");
        let i = starts.partition_point(|s| *s <= t).saturating_sub(1);
        sums[i].add(r);
    }
    let mut running = before.clone();
    let items: Vec<Value> = starts
        .iter()
        .zip(&sums)
        .map(|(s, b)| {
            running.merge(b);
            let mut v = b.json(&cfg);
            v["start"] = rfc3339(*s).into();
            v["end"] = rfc3339(bucket_next(kind, *s)).into();
            v["running"] = running.json(&cfg);
            v
        })
        .collect();
    Ok(Response::ok(json!({
        "game": g.as_str(), "discord_user_id": user.discord_user_id, "bucket": kind,
        "from": rfc3339(first), "to": rfc3339(end), "currency": jobs::currency(g),
        "before": before.json(&cfg), "items": items,
    })))
}

// ---- leaderboards (§5) -----------------------------------------------------------

/// `[start, end)` of a period, and its `at` label.
pub fn period(kind: &str, at: Option<&str>, now: i64) -> ApiResult<(Option<(i64, i64)>, Value)> {
    let bad = || ApiError::bad_request("bad period or at").details(json!({"period": kind, "at": at}));
    match kind {
        "all" => Ok((None, Value::Null)),
        "month" => {
            let (y, m) = match at {
                Some(s) => {
                    let (y, m) = s.split_once('-').ok_or_else(bad)?;
                    let (y, m) = (y.parse::<i64>().map_err(|_| bad())?, m.parse::<u32>().map_err(|_| bad())?);
                    if !(1..=12).contains(&m) || s.len() != 7 {
                        return Err(bad());
                    }
                    (y, m)
                }
                None => util::year_month_of(now),
            };
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            Ok((Some((util::month_start(y, m), util::month_start(ny, nm))), format!("{y:04}-{m:02}").into()))
        }
        "week" => {
            let (y, w) = match at {
                Some(s) => {
                    let (y, w) = s.split_once("-W").ok_or_else(bad)?;
                    let (y, w) = (y.parse::<i64>().map_err(|_| bad())?, w.parse::<u32>().map_err(|_| bad())?);
                    if w == 0 || w > util::iso_weeks_in_year(y) {
                        return Err(bad());
                    }
                    (y, w)
                }
                None => util::iso_week_of(now),
            };
            let start = util::iso_week_start(y, w);
            Ok((Some((start, start + 7 * 86_400_000)), format!("{y:04}-W{w:02}").into()))
        }
        _ => Err(bad()),
    }
}

pub async fn leaderboard<H: Host>(h: &H, req: &Request, g: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    let g = game(g)?;
    let now = h.now_ms();
    let kind = req.q("period").unwrap_or("all");
    let (range, at) = period(kind, req.q("at"), now)?;
    let division = req.q("division").unwrap_or("all");
    if !matches!(division, "all" | "singleplayer") {
        return Err(ApiError::bad_request("division is all or singleplayer"));
    }
    let (limit, offset) = http::page(req, now)?;
    // What to rank by (owner, 2026-10-15); damage ranks the least first.
    let sort = req.q("sort").unwrap_or("points");
    if !matches!(sort, "points" | "distance" | "game_distance" | "fuel" | "income" | "damage") {
        return Err(ApiError::bad_request("sort is points, distance, game_distance, fuel, income or damage").details(json!({"sort": sort})));
    }
    let order = req.q("order").unwrap_or(if sort == "damage" { "asc" } else { "desc" });
    if !matches!(order, "asc" | "desc") {
        return Err(ApiError::bad_request("order is asc or desc"));
    }
    let (from, to) = range.unwrap_or((i64::MIN, i64::MAX));
    let rows = h
        .all(
            &format!(
                "SELECT u.user_id, u.discord_user_id, u.display_name, SUM(j.points) AS points, COUNT(*) AS n,
                        SUM(j.world_km) AS world_km, SUM(j.game_km) AS game_km, MAX(j.delivered_at) AS reached,
                        COALESCE(SUM(json_extract(j.summary, '$.fuel_used_l')), 0) AS fuel,
                        COALESCE(SUM(json_extract(j.summary, '$.income')), 0) AS income,
                        AVG(json_extract(j.summary, '$.damage_pct')) AS damage
                 FROM jobs j JOIN users u ON u.user_id = j.user_id
                 WHERE j.game = ?1 AND j.outcome IN ('valid', 'approved') AND u.disabled = 0
                   AND j.delivered_at >= ?2 AND j.delivered_at < ?3 {}
                 GROUP BY u.user_id",
                if division == "singleplayer" { "AND u.singleplayer_division = 1" } else { "" }
            ),
            params![g.as_str(), from, to],
        )
        .await?;
    let cfg = config::scoring(h).await?.cfg;
    // (value ranked by, reached, user, item); players with no damage data last.
    let mut board: Vec<(Option<f64>, i64, String, Value)> = rows
        .iter()
        .map(|r| {
            let pts = cfg.rules.floor_points(r.f("points"));
            let (w, gk) = (r.f("world_km"), r.f("game_km"));
            let damage = r.opt_f("damage").map(round2);
            let item = json!({"user_id": r.s("user_id"), "discord_user_id": r.s("discord_user_id"),
                "display_name": r.s("display_name"), "points": pts, "job_count": r.i("n"),
                "world_km": round2(w), "world_mi": mi(w), "game_km": round2(gk), "game_mi": mi(gk),
                "fuel_used_l": round2(r.f("fuel")), "income": r.f("income").round() as i64, "damage_pct_avg": damage});
            let key = match sort {
                "distance" => Some(round2(w)),
                "game_distance" => Some(round2(gk)),
                "fuel" => Some(round2(r.f("fuel"))),
                "income" => Some(r.f("income").round()),
                "damage" => damage,
                _ => Some(pts),
            };
            (key, r.i("reached"), r.s("user_id"), item)
        })
        .collect();
    board.sort_by(|a, b| {
        let by = match (a.0, b.0) {
            (Some(x), Some(y)) if order == "asc" => x.total_cmp(&y),
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        by.then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
    });
    let total = board.len();
    let mut items = Vec::new();
    let mut rank = 0;
    for (i, (key, _, _, item)) in board.iter().enumerate() {
        if i == 0 || *key != board[i - 1].0 {
            rank = i + 1;
        }
        if i >= offset && i < offset + limit {
            let mut item = item.clone();
            item["rank"] = rank.into();
            items.push(item);
        }
    }
    let mut v = http::list(items, offset, limit, Some(total), offset + limit < total, now);
    v["game"] = g.as_str().into();
    v["period"] = kind.into();
    v["at"] = at;
    v["division"] = division.into();
    v["sort"] = sort.into();
    v["order"] = order.into();
    v["currency"] = jobs::currency(g).into();
    v["period_start"] = range.map_or(Value::Null, |r| util::rfc3339(r.0).into());
    v["period_end"] = range.map_or(Value::Null, |r| util::rfc3339(r.1).into());
    v["generated_at"] = util::rfc3339(now).into();
    Ok(Response::ok(v))
}
