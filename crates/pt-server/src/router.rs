//! Path → handler. Every response, success or error, goes through [`handle`].

use crate::env::Host;
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::{admin, ingest, manage, manual, public, route, updates};

pub async fn handle<H: Host>(h: &H, req: Request) -> Response {
    match dispatch(h, &req).await {
        Ok(r) => r,
        Err(e) => e.response(),
    }
}

async fn dispatch<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let segs: Vec<String> = req.path.trim_matches('/').split('/').map(crate::http::percent_decode).collect();
    let s: Vec<&str> = segs.iter().map(String::as_str).collect();
    let m = req.method.as_str();
    match (m, s.as_slice()) {
        ("GET", [""]) => Ok(Response::ok(serde_json::json!({"service": "plasmatrack", "api": "v1"}))),
        ("POST", ["api", "v1", "chunks"]) => ingest::upload(h, req).await,
        ("GET", ["dl", "trace", id]) => admin::download_trace(h, req, id).await,
        ("GET", ["dl", "evidence", id]) => manual::download(h, req, id).await,
        ("GET", ["dl", "releases", rest @ ..]) => updates::file(h, req, rest).await,
        ("GET", ["dl", "uninstall"]) => updates::uninstall(h, "windows").await,
        ("GET", ["dl", "uninstall", p @ ("windows" | "linux")]) => updates::uninstall(h, p).await,
        ("GET", ["api", "v1", "client", "update"]) => updates::client_update(h, req).await,

        // Player self-service.
        ("POST", ["api", "v1", "players", d, "token"]) => public::issue_token(h, req, d).await,
        ("GET", ["api", "v1", "players", d, "token"]) => public::token_status(h, req, d).await,
        ("DELETE", ["api", "v1", "players", d, "token"]) => public::revoke_token(h, req, d).await,
        ("PATCH", ["api", "v1", "players", d]) => public::rename(h, req, d).await,
        ("POST", ["api", "v1", "players", d, "evidence"]) => manual::upload(h, req, d).await,
        ("POST", ["api", "v1", "players", d, "manual-jobs"]) => manual::submit(h, req, d).await,
        ("GET", ["api", "v1", "players", d, "jobs", id, "evidence"]) => manual::player_evidence(h, req, d, id).await,
        ("GET", ["api", "v1", "players", d, "client-download"]) => updates::client_download(h, req, d).await,

        // Public reads.
        ("GET", ["api", "v1", "public", "client"]) => updates::latest(h, req).await,
        ("GET", ["api", "v1", "public", "players", d]) => public::player(h, req, d).await,
        ("GET", ["api", "v1", "public", "players", d, "jobs"]) => public::player_jobs_all(h, req, d).await,
        ("GET", ["api", "v1", "public", "jobs", id]) => public::job(h, req, id).await,
        ("GET", ["api", "v1", "public", "jobs", id, "route"]) => route::public(h, req, id).await,
        ("GET", ["api", "v1", "public", "jobs", id, "series"]) => admin::public_series(h, req, id).await,
        ("GET", ["api", "v1", "public", g, "players", d, "stats"]) => public::player_stats(h, req, g, d).await,
        ("GET", ["api", "v1", "public", g, "players", d, "jobs"]) => public::player_jobs(h, req, g, d).await,
        ("GET", ["api", "v1", "public", g, "players", d, "history"]) => public::player_history(h, req, g, d).await,
        ("GET", ["api", "v1", "public", g, "leaderboard"]) => public::leaderboard(h, req, g).await,

        // Moderators.
        ("GET", ["api", "v1", "admin", "review-queue"]) => admin::review_queue(h, req).await,
        ("GET", ["api", "v1", "admin", "review-queue", "count"]) => admin::review_queue_count(h, req).await,
        ("GET", ["api", "v1", "admin", "truckersmp", "flags"]) => admin::truckersmp_flags(h, req).await,
        ("GET", ["api", "v1", "admin", "jobs", id]) => admin::job_detail(h, req, id).await,
        ("PATCH", ["api", "v1", "admin", "jobs", id]) => admin::edit(h, req, id).await,
        ("GET", ["api", "v1", "admin", "jobs", id, "series"]) => admin::series(h, req, id).await,
        ("GET", ["api", "v1", "admin", "jobs", id, "route"]) => route::admin(h, req, id).await,
        ("GET", ["api", "v1", "admin", "jobs", id, "trace"]) => admin::trace(h, req, id).await,
        ("POST", ["api", "v1", "admin", "jobs", id, "recalculate"]) => admin::recalculate_job(h, req, id).await,
        ("POST", ["api", "v1", "admin", "jobs", id, a @ ("approve" | "reject" | "void" | "restore")]) => admin::decide(h, req, id, a).await,
        ("POST", ["api", "v1", "admin", "recalculations"]) => manage::recalculations(h, req).await,
        ("GET", ["api", "v1", "admin", "operations", id]) => manage::operation(h, req, id).await,
        ("GET", ["api", "v1", "admin", "bonuses"]) => manage::bonuses_list(h, req).await,
        ("POST", ["api", "v1", "admin", "bonuses"]) => manage::bonus_create(h, req).await,
        ("GET", ["api", "v1", "admin", "bonuses", id]) => manage::bonus_get(h, req, id).await,
        ("PATCH", ["api", "v1", "admin", "bonuses", id]) => manage::bonus_update(h, req, id).await,
        ("DELETE", ["api", "v1", "admin", "bonuses", id]) => manage::bonus_archive(h, req, id).await,
        ("GET", ["api", "v1", "admin", "players", d]) => manage::player(h, req, d).await,
        ("PATCH", ["api", "v1", "admin", "players", d]) => manage::player_update(h, req, d).await,
        ("POST", ["api", "v1", "admin", "players", d, "token"]) => manage::player_issue_token(h, req, d).await,
        ("DELETE", ["api", "v1", "admin", "players", d, "token"]) => manage::player_revoke_token(h, req, d).await,
        ("GET", ["api", "v1", "admin", "config-versions"]) => manage::config_list(h, req).await,
        ("POST", ["api", "v1", "admin", "config-versions"]) => manage::config_create(h, req).await,
        ("POST", ["api", "v1", "admin", "config-versions", id, "make-current"]) => manage::config_make_current(h, req, id).await,
        ("GET", ["api", "v1", "admin", "releases"]) => manage::release_list(h, req).await,
        ("POST", ["api", "v1", "admin", "releases"]) => manage::release_create(h, req).await,
        ("POST", ["api", "v1", "admin", "releases", id, "offer"]) => manage::release_offer(h, req, id).await,
        ("GET", ["api", "v1", "admin", "revocations"]) => manage::revocation_list(h, req).await,
        ("POST", ["api", "v1", "admin", "revocations"]) => manage::revocation_create(h, req).await,
        ("POST", ["api", "v1", "admin", "revocations", id, "cancel"]) => manage::revocation_cancel(h, req, id).await,
        ("GET", ["api", "v1", "admin", "admins"]) => manage::admins_list(h, req).await,
        ("POST", ["api", "v1", "admin", "admins"]) => manage::admin_add(h, req).await,
        ("DELETE", ["api", "v1", "admin", "admins", d]) => manage::admin_remove(h, req, d).await,
        ("GET", ["api", "v1", "admin", "audit"]) => admin::audit_log(h, req).await,
        ("POST", ["api", "v1", "admin", "backups"]) => crate::backup::now(h, req).await,

        _ => Err(ApiError::not_found("endpoint")),
    }
}
