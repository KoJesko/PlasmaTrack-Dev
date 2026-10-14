//! Who is calling (API_CONVENTIONS §2): the read token, the bot token, a
//! moderator acting through the bot, or a player's client.

use crate::env::{Host, RowExt};
use crate::http::{ApiError, ApiResult, Request};
use crate::params;
use crate::store::{self, User};
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Read,
    Bot,
}

fn bearer(req: &Request) -> Option<&str> {
    let v = req.header("authorization")?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn matches(hashes: &[String], token: &str) -> bool {
    let digest = util::sha256_hex(token.as_bytes());
    hashes.iter().any(|h| util::constant_eq(h.as_bytes(), digest.as_bytes()))
}

/// The read token or the bot token.
pub fn service<H: Host>(h: &H, req: &Request) -> ApiResult<Service> {
    let token = bearer(req).ok_or_else(ApiError::unauthorized)?;
    let s = h.settings();
    if matches(&s.bot_token_sha256, token) {
        Ok(Service::Bot)
    } else if matches(&s.read_token_sha256, token) {
        Ok(Service::Read)
    } else {
        Err(ApiError::unauthorized())
    }
}

pub fn bot<H: Host>(h: &H, req: &Request) -> ApiResult<()> {
    match service(h, req)? {
        Service::Bot => Ok(()),
        Service::Read => Err(ApiError::forbidden("The read token cannot do this.")),
    }
}

/// The moderator in `X-PlasmaTrack-Actor`: an admin who is not disabled. The
/// Discord IDs in the Worker's `BOOTSTRAP_ADMINS` are admins from their first
/// call (so the first admin can exist at all).
pub async fn admin<H: Host>(h: &H, req: &Request) -> ApiResult<User> {
    bot(h, req)?;
    let actor = req.header("x-plasmatrack-actor").unwrap_or_default().trim().to_string();
    if actor.is_empty() {
        return Err(ApiError::forbidden("X-PlasmaTrack-Actor is required on admin calls."));
    }
    store::check_discord_id(&actor)?;
    if h.settings().bootstrap_admins.contains(&actor) {
        let (u, _) = store::ensure_user(h, &actor, None).await?;
        if u.role != "admin" {
            h.run("UPDATE users SET role = 'admin' WHERE user_id = ?1", params![&u.user_id]).await?;
        }
        return Ok(User { role: "admin".into(), ..u });
    }
    match store::user_by_discord(h, &actor).await? {
        Some(u) if u.role == "admin" && !u.disabled => Ok(u),
        _ => Err(ApiError::forbidden("The actor is not an active admin.")),
    }
}

/// The player whose client uploads (`Authorization: Bearer pt_live_…`).
/// Disabled players get 401, like a revoked token (ADMIN_API §10).
pub async fn player<H: Host>(h: &H, req: &Request) -> ApiResult<User> {
    let token = bearer(req).ok_or_else(ApiError::unauthorized)?;
    let row = store::one(
        h,
        "SELECT t.token_id, t.last_used_at, u.user_id FROM tokens t JOIN users u ON u.user_id = t.user_id
         WHERE t.token_sha256 = ?1 AND t.revoked_at IS NULL AND u.disabled = 0",
        params![util::sha256_hex(token.as_bytes())],
    )
    .await?
    .ok_or_else(ApiError::unauthorized)?;
    let now = h.now_ms();
    // last_used_at to the minute is plenty and saves a write per upload.
    if now - row.opt_i("last_used_at").unwrap_or(0) > 60_000 {
        h.run("UPDATE tokens SET last_used_at = ?2 WHERE token_id = ?1", params![row.s("token_id"), now]).await?;
    }
    store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(ApiError::unauthorized)
}
