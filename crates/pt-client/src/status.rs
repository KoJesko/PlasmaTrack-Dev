//! Live client status, shared by all tasks, and the local status page
//! (`http://127.0.0.1:<status_port>/`, loopback only).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::sync::Notify;

use crate::db::{Db, DbStats};
use crate::live::{JobView, LiveJobs};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub client_version: String,
    pub plugin_connections: u32,
    pub last_frame_ms: Option<i64>,
    pub frames_received: u64,
    pub acks_sent: u64,
    pub last_session: Option<String>,
    pub last_game: Option<String>,
    pub upload_configured: bool,
    pub last_upload_attempt_ms: Option<i64>,
    pub last_upload_ok_ms: Option<i64>,
    pub last_upload_error: Option<String>,
    pub config_hash: String,
    pub manifest_files: usize,
    /// A newer release being downloaded or waiting to install (docs/UPDATES.md).
    pub update: Option<String>,
}

pub type SharedStatus = Arc<Mutex<Status>>;

#[derive(Clone)]
struct AppState {
    status: SharedStatus,
    db: Arc<Mutex<Db>>,
    shutdown: Arc<Notify>,
    jobs: Option<Arc<Mutex<LiveJobs>>>,
}

#[derive(Serialize)]
struct Report {
    #[serde(flatten)]
    status: Status,
    db: DbStats,
}

fn report(state: &AppState) -> Report {
    let status = state.status.lock().expect("status lock").clone();
    let db = state.db.lock().expect("db lock").stats().unwrap_or_default();
    Report { status, db }
}

async fn status_json(State(state): State<AppState>) -> impl IntoResponse {
    Json(report(&state))
}

fn jobs(state: &AppState) -> Vec<JobView> {
    state.jobs.as_ref().map(|j| j.lock().expect("live lock").jobs()).unwrap_or_default()
}

/// Provisional results (the server decides; PLAN §6b).
async fn jobs_json(State(state): State<AppState>) -> impl IntoResponse {
    Json(jobs(&state))
}

fn when(ms: i64) -> String {
    let age_s = (crate::now_ms() - ms).max(0) / 1000;
    match age_s {
        s if s < 120 => format!("{s} s ago"),
        s if s < 7200 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

fn job_rows(list: &[JobView]) -> String {
    if list.is_empty() {
        return "<p>No jobs yet. Jobs appear here as you drive.</p>".into();
    }
    let mut rows = String::from(
        "<table class=jobs><tr><th>Started</th><th>Game</th><th>Cargo</th><th>Route</th><th>State</th>
         <th>Result</th><th>Points</th><th>1:1 km</th><th>Game km</th><th>Notes</th></tr>",
    );
    for j in list {
        let game = match j.game {
            pt_core::Game::Ets2 => "ETS2",
            pt_core::Game::Ats => "ATS",
        };
        let enum_name = |v: serde_json::Value| v.as_str().unwrap_or_default().replace('_', " ");
        let mut notes = j.reasons.join(", ");
        if j.start == pt_core::timeline::StartKind::LateStart && !notes.contains("late_start") {
            notes = if notes.is_empty() { "late start".into() } else { format!("late start, {notes}") };
        }
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{game}</td><td>{}</td><td>{} → {}</td><td>{}</td><td>{}</td><td>{:.1}</td>
             <td>{:.1}</td><td>{:.0}</td><td>{}</td></tr>",
            when(j.started_ms),
            esc(&j.cargo),
            esc(&j.source_city),
            esc(&j.destination_city),
            enum_name(serde_json::to_value(j.state).unwrap_or_default()),
            enum_name(serde_json::to_value(j.outcome).unwrap_or_default()),
            j.points,
            j.world_km,
            j.game_km,
            esc(&notes.replace('_', " ")),
        ));
    }
    rows.push_str("</table>");
    rows
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

async fn status_page(State(state): State<AppState>) -> impl IntoResponse {
    let r = report(&state);
    let job_table = job_rows(&jobs(&state));
    let json = serde_json::to_value(&r).unwrap_or_default();
    let mut rows = String::new();
    if let Some(obj) = json.as_object() {
        for (k, v) in obj {
            let v = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            rows.push_str(&format!("<tr><th>{}</th><td>{}</td></tr>", esc(k), esc(&v)));
        }
    }
    Html(format!(
        "<!doctype html><html><head><meta charset=utf-8><meta http-equiv=refresh content=5>\
         <title>PlasmaTrack client</title><style>body{{font-family:system-ui,sans-serif;margin:16px}}\
         th{{text-align:left;padding-right:16px}}td{{font-family:monospace}}\
         .jobs td,.jobs th{{padding:2px 12px 2px 0;font-family:system-ui,sans-serif}}</style></head>\
         <body><h1>PlasmaTrack client</h1><h2>Jobs</h2>\
         <p>Provisional: the server's result is the one that counts.</p>{job_table}\
         <h2>Client</h2><table>{rows}</table></body></html>"
    ))
}

/// Header `plasmatrack-client stop` sends with `POST /quit`.
pub const QUIT_HEADER: (&str, &str) = ("x-plasmatrack", "quit");

/// Stops the client (`plasmatrack-client stop`, the installer). Browsers cannot
/// trigger it: they send `Origin` on cross-site POSTs, and the custom header
/// needs a CORS preflight this server never grants.
async fn quit(State(state): State<AppState>, headers: HeaderMap) -> StatusCode {
    let asked = headers.get(QUIT_HEADER.0).is_some_and(|v| v == QUIT_HEADER.1);
    if !asked || headers.contains_key("origin") {
        return StatusCode::FORBIDDEN;
    }
    state.shutdown.notify_one();
    StatusCode::NO_CONTENT
}

pub fn router(status: SharedStatus, db: Arc<Mutex<Db>>, shutdown: Arc<Notify>, jobs: Option<Arc<Mutex<LiveJobs>>>) -> Router {
    Router::new()
        .route("/", get(status_page))
        .route("/status.json", get(status_json))
        .route("/jobs.json", get(jobs_json))
        .route("/quit", post(quit))
        .with_state(AppState { status, db, shutdown, jobs })
}

/// Serves the status page until the task is dropped. Only loopback addresses
/// are accepted by the config validation.
pub async fn serve(
    addr: SocketAddr,
    status: SharedStatus,
    db: Arc<Mutex<Db>>,
    shutdown: Arc<Notify>,
    jobs: Option<Arc<Mutex<LiveJobs>>>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(status, db, shutdown, jobs)).await
}
