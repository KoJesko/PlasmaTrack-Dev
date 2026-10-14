//! Shared storage helpers: ids, users, tokens, the audit log and idempotency
//! (API_CONVENTIONS §3).

use serde_json::{json, Value};

use crate::env::{Host, Row, RowExt, P};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::params;
use crate::util;

/// `prefix_` + 10 Crockford characters (50 random bits), lower-case.
pub fn new_id<H: Host>(h: &H, prefix: &str) -> String {
    let mut b = [0u8; 7];
    h.random(&mut b);
    format!("{prefix}_{}", util::crockford(&b, 10).to_ascii_lowercase())
}

/// An 8-character job id (40 random bits).
pub fn new_job_id<H: Host>(h: &H) -> String {
    let mut b = [0u8; 5];
    h.random(&mut b);
    util::crockford(&b, 8)
}

/// A player token: `pt_live_` + 43 base64url characters (256 bits).
pub fn new_token<H: Host>(h: &H) -> String {
    let mut b = [0u8; 32];
    h.random(&mut b);
    format!("pt_live_{}", util::b64url(&b))
}

pub async fn one<H: Host>(h: &H, sql: &str, p: Vec<P>) -> ApiResult<Option<Row>> {
    Ok(h.all(sql, p).await?.into_iter().next())
}

pub async fn count<H: Host>(h: &H, sql: &str, p: Vec<P>) -> ApiResult<usize> {
    Ok(one(h, sql, p).await?.map(|r| r.values().next().and_then(Value::as_f64).unwrap_or(0.0) as usize).unwrap_or(0))
}

// ---- users -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub user_id: String,
    pub discord_user_id: String,
    pub display_name: String,
    pub role: String,
    pub singleplayer_division: bool,
    pub division_changed_at: i64,
    pub disabled: bool,
    pub created_at: i64,
}

impl User {
    fn from_row(r: &Row) -> User {
        User {
            user_id: r.s("user_id"),
            discord_user_id: r.s("discord_user_id"),
            display_name: r.s("display_name"),
            role: r.s("role"),
            singleplayer_division: r.b("singleplayer_division"),
            division_changed_at: r.i("division_changed_at"),
            disabled: r.b("disabled"),
            created_at: r.i("created_at"),
        }
    }

    /// `{"user_id", "discord_user_id", "display_name"}`.
    pub fn brief(&self) -> Value {
        json!({"user_id": self.user_id, "discord_user_id": self.discord_user_id, "display_name": self.display_name})
    }
}

pub async fn user_by_discord<H: Host>(h: &H, discord_id: &str) -> ApiResult<Option<User>> {
    Ok(one(h, "SELECT * FROM users WHERE discord_user_id = ?1", params![discord_id]).await?.map(|r| User::from_row(&r)))
}

pub async fn user_by_id<H: Host>(h: &H, user_id: &str) -> ApiResult<Option<User>> {
    Ok(one(h, "SELECT * FROM users WHERE user_id = ?1", params![user_id]).await?.map(|r| User::from_row(&r)))
}

/// Discord IDs are decimal snowflakes.
pub fn check_discord_id(id: &str) -> ApiResult<()> {
    if (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ApiError::bad_request("Discord IDs are decimal strings.").details(json!({"discord_id": id})))
    }
}

pub fn check_display_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    let n = name.chars().count();
    if !(2..=32).contains(&n) {
        return Err(ApiError::field("display_name", "must be 2-32 characters"));
    }
    Ok(name.to_string())
}

/// The user of a Discord ID, created when missing. Returns (user, created).
pub async fn ensure_user<H: Host>(h: &H, discord_id: &str, display_name: Option<&str>) -> ApiResult<(User, bool)> {
    if let Some(u) = user_by_discord(h, discord_id).await? {
        return Ok((u, false));
    }
    let name = match display_name {
        Some(n) => check_display_name(n)?,
        None => format!("Player {}", &discord_id[discord_id.len().saturating_sub(4)..]),
    };
    let id = new_id(h, "usr");
    h.run(
        "INSERT OR IGNORE INTO users (user_id, discord_user_id, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&id, discord_id, &name, h.now_ms()],
    )
    .await?;
    let u = user_by_discord(h, discord_id).await?.ok_or_else(|| ApiError::internal("user vanished"))?;
    let created = u.user_id == id;
    Ok((u, created))
}

// ---- tokens ----------------------------------------------------------------------

/// The active token's metadata, if any.
pub async fn active_token<H: Host>(h: &H, user_id: &str) -> ApiResult<Option<Value>> {
    Ok(one(
        h,
        "SELECT token_id, created_at, last_used_at FROM tokens WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY created_at DESC LIMIT 1",
        params![user_id],
    )
    .await?
    .map(|r| {
        json!({"token_id": r.s("token_id"), "created_at": util::rfc3339(r.i("created_at")),
               "last_used_at": util::opt_rfc3339(r.opt_i("last_used_at"))})
    }))
}

pub const TOKENS_PER_DAY: usize = 5;

/// Issues a token, revoking the previous one. Returns the response object
/// with the plaintext (PUBLIC_API §1) and the audit "after".
pub async fn issue_token<H: Host>(h: &H, user: &User) -> ApiResult<(Value, Option<String>)> {
    let now = h.now_ms();
    let today = count(h, "SELECT COUNT(*) AS n FROM tokens WHERE user_id = ?1 AND created_at > ?2", params![&user.user_id, now - 86_400_000])
        .await?;
    if today >= TOKENS_PER_DAY {
        return Err(ApiError::rate_limited(3600));
    }
    let previous = one(h, "SELECT token_id FROM tokens WHERE user_id = ?1 AND revoked_at IS NULL", params![&user.user_id])
        .await?
        .map(|r| r.s("token_id"));
    let token = new_token(h);
    let token_id = new_id(h, "tok");
    h.batch(vec![
        ("UPDATE tokens SET revoked_at = ?2 WHERE user_id = ?1 AND revoked_at IS NULL".into(), params![&user.user_id, now]),
        (
            "INSERT INTO tokens (token_id, user_id, token_sha256, created_at) VALUES (?1, ?2, ?3, ?4)".into(),
            params![&token_id, &user.user_id, util::sha256_hex(token.as_bytes()), now],
        ),
    ])
    .await?;
    Ok((
        json!({"token_id": token_id, "token": token, "created_at": util::rfc3339(now), "revoked_token_id": previous}),
        previous,
    ))
}

pub async fn revoke_token<H: Host>(h: &H, user: &User) -> ApiResult<String> {
    let Some(r) = one(h, "SELECT token_id FROM tokens WHERE user_id = ?1 AND revoked_at IS NULL", params![&user.user_id]).await? else {
        return Err(ApiError::not_found("token"));
    };
    h.run("UPDATE tokens SET revoked_at = ?2 WHERE token_id = ?1", params![r.s("token_id"), h.now_ms()]).await?;
    Ok(r.s("token_id"))
}

// ---- audit -----------------------------------------------------------------------

pub struct Audit<'a> {
    pub actor: &'a str,
    pub action: &'a str,
    pub target_kind: &'a str,
    pub target_id: &'a str,
    pub target_user: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub before: Value,
    pub after: Value,
}

pub fn audit_stmt<H: Host>(h: &H, a: Audit<'_>) -> (String, Vec<P>) {
    (
        "INSERT INTO audit_log (at, actor, action, target_kind, target_id, target_user, reason, before, after)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            .into(),
        params![
            h.now_ms(),
            a.actor,
            a.action,
            a.target_kind,
            a.target_id,
            a.target_user,
            a.reason,
            a.before.to_string(),
            a.after.to_string()
        ],
    )
}

pub async fn audit<H: Host>(h: &H, a: Audit<'_>) -> ApiResult<()> {
    let (sql, p) = audit_stmt(h, a);
    h.run(&sql, p).await?;
    Ok(())
}

// ---- idempotency -----------------------------------------------------------------

pub const IDEMPOTENCY_TTL_MS: i64 = 24 * 3_600_000;

pub struct Idem {
    scope: String,
    key: String,
    body_sha256: String,
}

/// Checks `Idempotency-Key` on a write. `Ok(Err(response))` is a replay to
/// return as is; `Ok(Ok(idem))` means go ahead and [`idem_store`] the result.
pub async fn idem_begin<H: Host>(h: &H, req: &Request, scope: &str) -> ApiResult<Result<Idem, Response>> {
    let key = req.header("idempotency-key").unwrap_or_default();
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(ApiError::new(400, "validation_failed", "Writes need an Idempotency-Key header (1-128 printable ASCII).")
            .details(json!({"fields": {"idempotency_key": "required"}})));
    }
    let body_sha256 = util::sha256_hex(&req.body);
    let found = one(
        h,
        "SELECT body_sha256, status, response, created_at FROM idempotency WHERE scope = ?1 AND key = ?2",
        params![scope, key],
    )
    .await?;
    if let Some(r) = found {
        if h.now_ms() - r.i("created_at") <= IDEMPOTENCY_TTL_MS {
            if r.s("body_sha256") != body_sha256 {
                return Err(ApiError::new(409, "idempotency_mismatch", "This Idempotency-Key was used with a different body."));
            }
            return Ok(Err(Response::json(r.i("status") as u16, &r.json("response"))));
        }
    }
    Ok(Ok(Idem { scope: scope.into(), key: key.into(), body_sha256 }))
}

/// Remembers the response a replay should get (it may differ from the one
/// sent now: a token's plaintext is never replayed).
pub async fn idem_store<H: Host>(h: &H, idem: Idem, status: u16, replay_body: &Value) -> ApiResult<()> {
    h.run(
        "INSERT OR REPLACE INTO idempotency (scope, key, body_sha256, status, response, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![idem.scope, idem.key, idem.body_sha256, status as i64, replay_body.to_string(), h.now_ms()],
    )
    .await?;
    Ok(())
}

/// The `reason` of an admin write: 3-1000 characters after trimming.
pub fn reason(body: &Value) -> ApiResult<String> {
    let r = body.get("reason").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let n = r.chars().count();
    if n == 0 {
        return Err(ApiError::field("reason", "required"));
    }
    if !(3..=1000).contains(&n) {
        return Err(ApiError::field("reason", "must be 3-1000 characters"));
    }
    Ok(r.to_string())
}
