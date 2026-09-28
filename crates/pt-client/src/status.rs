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
}

pub type SharedStatus = Arc<Mutex<Status>>;

#[derive(Clone)]
struct AppState {
    status: SharedStatus,
    db: Arc<Mutex<Db>>,
    shutdown: Arc<Notify>,
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

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

async fn status_page(State(state): State<AppState>) -> impl IntoResponse {
    let r = report(&state);
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
        "<!doctype html><html><head><meta charset=utf-8><meta http-equiv=refresh content=2>\
         <title>PlasmaTrack client</title><style>body{{font-family:system-ui,sans-serif;margin:16px}}\
         th{{text-align:left;padding-right:16px}}td{{font-family:monospace}}</style></head>\
         <body><h1>PlasmaTrack client</h1><table>{rows}</table></body></html>"
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

pub fn router(status: SharedStatus, db: Arc<Mutex<Db>>, shutdown: Arc<Notify>) -> Router {
    Router::new()
        .route("/", get(status_page))
        .route("/status.json", get(status_json))
        .route("/quit", post(quit))
        .with_state(AppState { status, db, shutdown })
}

/// Serves the status page until the task is dropped. Only loopback addresses
/// are accepted by the config validation.
pub async fn serve(addr: SocketAddr, status: SharedStatus, db: Arc<Mutex<Db>>, shutdown: Arc<Notify>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(status, db, shutdown)).await
}
