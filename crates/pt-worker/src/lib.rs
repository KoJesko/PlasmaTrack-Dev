//! The PlasmaTrack server on Cloudflare Workers (PLAN §7a). All logic lives in
//! `pt-server`; this crate only adapts it to the bindings in `wrangler.toml`:
//!
//! - `DB` (D1), `RAW`, `BACKUPS` and `RELEASES` (R2), queues `WEBHOOKS` and `OPS`, Durable Object
//!   `SCORER` (one per player and game), and a Cron Trigger;
//! - variables `PUBLIC_URL`, `WEBHOOK_URL`, `BOOTSTRAP_ADMINS`;
//! - secrets `READ_TOKEN`, `BOT_TOKEN`, `WEBHOOK_SECRETS`, `DOWNLOAD_SECRET`
//!   (comma-separated lists where rotation needs two values at once).

use std::cell::{Cell, RefCell};
use std::time::Duration;

use futures_util::future::{select, Either};
use pt_core::Game;
use pt_server::env::{Host, HostError, HostResult, LocalSql, Row, Settings, SqlVal, Stmt, P};
use pt_server::http;
use pt_server::scorer::Scorer;
use pt_server::webhooks::Delivery;
use pt_server::{manage, updates, util, webhooks};
use serde_json::{json, Value};
use worker::wasm_bindgen::JsValue;
use worker::*;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);
/// A pass that failed (R2, D1) is tried again this soon.
const RETRY_MS: i64 = 60_000;
/// When a pass left chunks (`Report::more`), the next starts this soon, as a
/// new invocation with its own limits.
const NEXT_PASS_MS: i64 = 1_000;

/// A variable or secret (either works for every name), split at commas.
fn list(env: &Env, name: &str, _secret: bool) -> Vec<String> {
    let v = env.var(name).map(|s| s.to_string()).or_else(|_| env.secret(name).map(|s| s.to_string()));
    v.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn settings(env: &Env) -> Settings {
    let hashes = |name| list(env, name, true).iter().map(|t| util::sha256_hex(t.as_bytes())).collect();
    Settings {
        read_token_sha256: hashes("READ_TOKEN"),
        bot_token_sha256: hashes("BOT_TOKEN"),
        bootstrap_admins: list(env, "BOOTSTRAP_ADMINS", false),
        webhook_url: list(env, "WEBHOOK_URL", false).into_iter().next(),
        webhook_secrets: list(env, "WEBHOOK_SECRETS", true),
        public_url: list(env, "PUBLIC_URL", false).into_iter().next().unwrap_or_default(),
        download_secret: list(env, "DOWNLOAD_SECRET", true).into_iter().next().unwrap_or_default(),
        backup_prefix: list(env, "BACKUP_PREFIX", false).into_iter().next().unwrap_or_default(),
        update_keys: Vec::new(),
        min_client_version: list(env, "MIN_CLIENT_VERSION", false).first().and_then(|v| pt_core::update::parse_version(v)),
    }
}

struct CfHost {
    env: Env,
    settings: Settings,
    /// Lets an upload hand the scorer poke off without waiting for it.
    ctx: Option<Context>,
}

fn host_err(e: impl std::fmt::Display) -> HostError {
    HostError(e.to_string())
}

fn js(p: &P) -> JsValue {
    match p {
        P::Null => JsValue::NULL,
        P::Int(i) => JsValue::from_f64(*i as f64),
        P::Real(f) => JsValue::from_f64(*f),
        P::Text(s) => JsValue::from_str(s),
    }
}

impl CfHost {
    fn new(env: Env, ctx: Option<Context>) -> CfHost {
        let settings = settings(&env);
        CfHost { env, settings, ctx }
    }

    fn statement(&self, sql: &str, params: &[P]) -> HostResult<D1PreparedStatement> {
        let db = self.env.d1("DB").map_err(host_err)?;
        let vals: Vec<JsValue> = params.iter().map(js).collect();
        db.prepare(sql).bind(&vals).map_err(host_err)
    }
}

impl Host for CfHost {
    fn settings(&self) -> &Settings {
        &self.settings
    }

    fn now_ms(&self) -> i64 {
        Date::now().as_millis() as i64
    }

    fn random(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("crypto.getRandomValues");
    }

    async fn all(&self, sql: &str, params: Vec<P>) -> HostResult<Vec<Row>> {
        let r = self.statement(sql, &params)?.all().await.map_err(host_err)?;
        let rows: Vec<Value> = r.results().map_err(host_err)?;
        Ok(rows.into_iter().filter_map(|v| if let Value::Object(o) = v { Some(o) } else { None }).collect())
    }

    async fn run(&self, sql: &str, params: Vec<P>) -> HostResult<u64> {
        let r = self.statement(sql, &params)?.run().await.map_err(host_err)?;
        Ok(r.meta().ok().flatten().and_then(|m| m.changes).unwrap_or(0) as u64)
    }

    async fn batch(&self, stmts: Vec<Stmt>) -> HostResult<()> {
        let db = self.env.d1("DB").map_err(host_err)?;
        let prepared = stmts.iter().map(|(sql, p)| self.statement(sql, p)).collect::<HostResult<Vec<_>>>()?;
        for r in db.batch(prepared).await.map_err(host_err)? {
            if let Some(e) = r.error() {
                return Err(HostError(e));
            }
        }
        Ok(())
    }

    async fn blob_get(&self, key: &str) -> HostResult<Option<Vec<u8>>> {
        let bucket = self.env.bucket("RAW").map_err(host_err)?;
        match bucket.get(key).execute().await.map_err(host_err)? {
            Some(obj) => match obj.body() {
                Some(b) => Ok(Some(b.bytes().await.map_err(host_err)?)),
                None => Ok(Some(Vec::new())),
            },
            None => Ok(None),
        }
    }

    async fn blob_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()> {
        let bucket = self.env.bucket("RAW").map_err(host_err)?;
        bucket.put(key, bytes).execute().await.map_err(host_err)?;
        Ok(())
    }

    async fn blob_delete(&self, key: &str) -> HostResult<()> {
        let bucket = self.env.bucket("RAW").map_err(host_err)?;
        bucket.delete(key).await.map_err(host_err)
    }

    async fn blob_size(&self, key: &str) -> HostResult<Option<u64>> {
        let bucket = self.env.bucket("RAW").map_err(host_err)?;
        Ok(bucket.head(key).await.map_err(host_err)?.map(|o| o.size()))
    }

    async fn backup_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()> {
        let bucket = self.env.bucket("BACKUPS").map_err(host_err)?;
        bucket.put(key, bytes).execute().await.map_err(host_err)?;
        Ok(())
    }

    async fn backup_list(&self, prefix: &str) -> HostResult<Vec<String>> {
        let bucket = self.env.bucket("BACKUPS").map_err(host_err)?;
        let mut keys = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut list = bucket.list().prefix(prefix);
            if let Some(c) = cursor.take() {
                list = list.cursor(c);
            }
            let page = list.execute().await.map_err(host_err)?;
            keys.extend(page.objects().iter().map(|o| o.key()));
            match page.cursor() {
                Some(c) if page.truncated() => cursor = Some(c),
                _ => return Ok(keys),
            }
        }
    }

    async fn backup_delete(&self, key: &str) -> HostResult<()> {
        let bucket = self.env.bucket("BACKUPS").map_err(host_err)?;
        bucket.delete(key).await.map_err(host_err)
    }

    async fn release_size(&self, key: &str) -> HostResult<Option<u64>> {
        let bucket = self.env.bucket("RELEASES").map_err(host_err)?;
        Ok(bucket.head(key).await.map_err(host_err)?.map(|o| o.size()))
    }

    async fn poke_scorer(&self, user_id: &str, game: &str) -> HostResult<()> {
        let ns = self.env.durable_object("SCORER").map_err(host_err)?;
        let name = format!("{user_id}:{game}");
        let url = format!("https://scorer/process?user={user_id}&game={game}");
        let poke = async move {
            let stub = ns.id_from_name(&name).and_then(|id| id.get_stub());
            match stub {
                Ok(s) => {
                    if let Err(e) = s.fetch_with_str(&url).await {
                        console_error!("scorer {name}: {e}");
                    }
                }
                Err(e) => console_error!("scorer {name}: {e}"),
            }
        };
        match &self.ctx {
            Some(ctx) => ctx.wait_until(poke),
            None => poke.await,
        }
        Ok(())
    }

    async fn queue_webhook(&self, event_id: &str, delay_s: u32) -> HostResult<()> {
        let q = self.env.queue("WEBHOOKS").map_err(host_err)?;
        // Plain strings: an object would arrive as a JS Map (serde-wasm-bindgen).
        q.send(MessageBuilder::new(event_id.to_string()).delay_seconds(delay_s).build()).await.map_err(host_err)
    }

    async fn queue_op(&self, msg: Value) -> HostResult<()> {
        let q = self.env.queue("OPS").map_err(host_err)?;
        q.send(msg.to_string()).await.map_err(host_err)
    }

    async fn http_post(&self, url: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> HostResult<u16> {
        let h = Headers::new();
        for (k, v) in &headers {
            h.set(k, v).map_err(host_err)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(h).with_body(Some(JsValue::from_str(&String::from_utf8_lossy(&body))));
        let req = Request::new_with_init(url, &init).map_err(host_err)?;
        let controller = AbortController::default();
        let signal = controller.signal();
        let fetch = Fetch::Request(req);
        let sending = Box::pin(fetch.send_with_signal(&signal));
        let timeout = Box::pin(Delay::from(WEBHOOK_TIMEOUT));
        let outcome = match select(sending, timeout).await {
            Either::Left((r, _)) => Some(r.map(|resp| resp.status_code())),
            Either::Right(_) => None,
        };
        match outcome {
            Some(Ok(status)) => Ok(status),
            Some(Err(e)) => Err(host_err(e)),
            None => {
                controller.abort();
                Err(HostError("timed out after 10 s".into()))
            }
        }
    }
}

async fn to_request(mut req: Request) -> Result<http::Request> {
    let url = req.url()?;
    let mut r = http::Request::new(req.method().as_ref(), url.path());
    r.query = http::parse_query(url.query().unwrap_or(""));
    r.headers = req.headers().entries().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
    r.body = req.bytes().await.unwrap_or_default();
    Ok(r)
}

fn to_response(r: http::Response) -> Result<Response> {
    let h = Headers::new();
    for (k, v) in &r.headers {
        h.set(k, v)?;
    }
    Ok(Response::from_bytes(r.body)?.with_status(r.status).with_headers(h))
}

/// A release file pt-server allowed (`updates::STREAM_HEADER`), streamed from
/// the release bucket: a setup program is too large to hold in memory. The
/// body is handed to the runtime as is, so it costs no CPU time and keeps its
/// length.
async fn stream_release(env: &Env, r: http::Response) -> Result<Response> {
    let key = r.headers.iter().find(|(k, _)| k == updates::STREAM_HEADER).map(|(_, v)| v.clone()).unwrap_or_default();
    let obj = env.bucket("RELEASES")?.get(&key).execute().await?;
    let Some(body) = obj.as_ref().and_then(|o| o.body()) else {
        return to_response(http::ApiError::not_found("file").response());
    };
    let h = Headers::new();
    for (k, v) in r.headers.iter().filter(|(k, _)| k != updates::STREAM_HEADER) {
        h.set(k, v)?;
    }
    Ok(Response::from_body(body.response_body()?)?.with_status(r.status).with_headers(h))
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let mut host = CfHost::new(env, Some(ctx));
    if host.settings.public_url.is_empty() {
        // Signed download links point back at whatever URL the bot called.
        let url = req.url()?;
        host.settings.public_url = url.origin().ascii_serialization();
    }
    let r = pt_server::handle(&host, to_request(req).await?).await;
    if r.headers.iter().any(|(k, _)| k == updates::STREAM_HEADER) {
        return stream_release(&host.env, r).await;
    }
    to_response(r)
}

/// Both queues: webhook deliveries (an `evt_…` id) and operation steps (JSON
/// text).
#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
    let host = CfHost::new(env, None);
    for msg in batch.messages()? {
        let text = match msg.body() {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if let Some(id) = text.strip_prefix("evt_").map(|_| text.as_str()) {
            match webhooks::deliver(&host, id).await {
                Ok(Delivery::Done | Delivery::Dropped) => msg.ack(),
                Ok(Delivery::RetryIn(s)) => msg.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(s).build()),
                Err(e) => {
                    console_error!("webhook {id}: {}", e.message);
                    msg.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(60).build());
                }
            }
        } else {
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            match manage::run_op(&host, &body).await {
                Ok(next) => {
                    if let Some(next) = next {
                        host.queue_op(next).await.map_err(|e| Error::RustError(e.0))?;
                    }
                    msg.ack();
                }
                Err(e) => {
                    console_error!("operation step: {}", e.message);
                    msg.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(60).build());
                }
            }
        }
    }
    Ok(())
}

/// The weekly backup, `47 3 * * SUN` in wrangler.toml (docs/SERVER.md); every
/// other schedule is the cron job. Matched by its time, so Cloudflare's
/// spelling of the weekday does not matter.
const BACKUP_CRON_TIME: &str = "47 3 ";

#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let host = CfHost::new(env, None);
    if event.cron().starts_with(BACKUP_CRON_TIME) {
        // Sizes and row counts only, never row contents.
        match pt_server::backup::run(&host).await {
            Ok(r) => console_log!("backup written: {} ({} bytes, {} tables, deleted {:?})", r.key, r.bytes, r.tables.len(), r.deleted),
            Err(e) => console_error!("backup failed: {}", e.message),
        }
    } else if let Err(e) = manage::cron(&host).await {
        console_error!("cron: {}", e.message);
    }
}

/// The object's own SQLite storage, where the scorer keeps its points and
/// state (pt_server::points).
#[derive(Clone)]
struct ObjectSql(SqlStorage);

impl LocalSql for ObjectSql {
    fn exec(&self, sql: &str, params: &[SqlVal]) -> HostResult<Vec<Vec<SqlVal>>> {
        let sql = sql.trim();
        if sql.is_empty() {
            return Ok(Vec::new());
        }
        let bindings: Vec<SqlStorageValue> = params
            .iter()
            .map(|p| match p {
                SqlVal::Null => SqlStorageValue::Null,
                SqlVal::Int(i) => SqlStorageValue::Integer(*i),
                SqlVal::Real(f) => SqlStorageValue::Float(*f),
                SqlVal::Text(s) => SqlStorageValue::String(s.clone()),
                SqlVal::Blob(b) => SqlStorageValue::Blob(b.clone()),
            })
            .collect();
        let cursor = self.0.exec(sql, bindings).map_err(host_err)?;
        let mut rows = Vec::new();
        for row in cursor.raw() {
            let row = row.map_err(host_err)?;
            rows.push(
                row.into_iter()
                    .map(|v| match v {
                        SqlStorageValue::Null => SqlVal::Null,
                        SqlStorageValue::Boolean(b) => SqlVal::Int(b as i64),
                        SqlStorageValue::Integer(i) => SqlVal::Int(i),
                        SqlStorageValue::Float(f) => SqlVal::Real(f),
                        SqlStorageValue::String(s) => SqlVal::Text(s),
                        SqlStorageValue::Blob(b) => SqlVal::Blob(b),
                    })
                    .collect(),
            );
        }
        Ok(rows)
    }
}

/// One per (player, game): runs that player's timeline (pt_server::scorer).
/// Requests are processed one at a time; a poke during a run makes the run
/// go round once more instead of overlapping it. Everything the scorer needs
/// to continue is committed to the object's SQLite after every chunk, so
/// nothing has to be saved when the object goes idle.
#[durable_object]
pub struct ScorerObject {
    state: State,
    env: Env,
    scorer: RefCell<Option<Scorer<ObjectSql>>>,
    busy: Cell<bool>,
    again: Cell<bool>,
    /// The last pass left chunks for the next one.
    more: Cell<bool>,
    /// (user, game) of this object, from its first request; the alarm needs it.
    who: RefCell<Option<(String, Game)>>,
}

impl ScorerObject {
    /// Processes until no poke arrived during the last run, or until a pass
    /// leaves chunks for the next invocation.
    async fn drain(&self, host: &CfHost, scorer: &mut Scorer<ObjectSql>) -> pt_server::http::ApiResult<pt_server::scorer::Report> {
        loop {
            self.again.set(false);
            let r = scorer.process(host).await;
            self.more.set(r.as_ref().is_ok_and(|r| r.more));
            if r.is_err() || self.more.get() || !self.again.get() {
                return r;
            }
        }
    }
}

impl DurableObject for ScorerObject {
    fn new(state: State, env: Env) -> Self {
        ScorerObject {
            state,
            env,
            scorer: RefCell::new(None),
            busy: Cell::new(false),
            again: Cell::new(false),
            more: Cell::new(false),
            who: RefCell::new(None),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let q: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let get = |k: &str| q.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let (user, game) = (get("user"), Game::from_api(&get("game")).ok_or_else(|| Error::RustError("bad game".into()))?);
        if self.who.borrow().as_ref() != Some(&(user.clone(), game)) {
            // Kept in storage too: an alarm can wake an evicted object.
            self.state.storage().put("who", (user.clone(), game.as_str().to_string())).await?;
            *self.who.borrow_mut() = Some((user.clone(), game));
        }
        if self.busy.get() {
            // The running pass (or the alarm) goes round once more.
            self.again.set(true);
            return Response::ok("queued");
        }
        self.busy.set(true);
        let host = CfHost::new(self.env.clone(), None);
        let taken = self.scorer.borrow_mut().take();
        let mut scorer = taken.unwrap_or_else(|| Scorer::new(&user, game, ObjectSql(self.state.storage().sql())));
        let result = self.drain(&host, &mut scorer).await;
        let (in_memory, on_disk) = scorer.footprint();
        *self.scorer.borrow_mut() = Some(scorer);
        self.busy.set(false);
        if self.more.get() || result.is_err() {
            // More chunks, or a failure (R2, D1): try again soon.
            let delay = if self.more.get() { NEXT_PASS_MS } else { RETRY_MS };
            self.state.storage().set_alarm(delay).await?;
        }
        match result {
            Ok(r) => Response::from_json(&json!({"chunks": r.chunks, "frames": r.frames, "jobs_stored": r.jobs_stored, "rebuilt": r.rebuilt,
                "points_in_memory": in_memory, "point_bytes_on_disk": on_disk})),
            Err(e) => {
                console_error!("scorer {user}: {}", e.message);
                Response::error(e.message, e.status)
            }
        }
    }

    /// The next pass of a long catch-up, or a retry after a failure.
    async fn alarm(&self) -> Result<Response> {
        if self.busy.get() {
            self.state.storage().set_alarm(NEXT_PASS_MS).await?;
            return Response::ok("busy");
        }
        let known = self.who.borrow().clone();
        let who = match known {
            Some(w) => Some(w),
            None => self
                .state
                .storage()
                .get::<(String, String)>("who")
                .await
                .ok()
                .flatten()
                .and_then(|(u, g)| Game::from_api(&g).map(|g| (u, g))),
        };
        let Some((user, game)) = who else { return Response::ok("idle") };
        *self.who.borrow_mut() = Some((user.clone(), game));
        let taken = self.scorer.borrow_mut().take();
        let mut scorer = taken.unwrap_or_else(|| Scorer::new(&user, game, ObjectSql(self.state.storage().sql())));
        self.busy.set(true);
        let host = CfHost::new(self.env.clone(), None);
        let outcome = self.drain(&host, &mut scorer).await;
        let (in_memory, on_disk) = scorer.footprint();
        *self.scorer.borrow_mut() = Some(scorer);
        self.busy.set(false);
        match outcome {
            Ok(r) => {
                console_log!("scorer {user}: {} chunks, {} jobs stored, {in_memory} points in memory, {on_disk} bytes on disk", r.chunks, r.jobs_stored);
                if self.more.get() {
                    self.state.storage().set_alarm(NEXT_PASS_MS).await?;
                    return Response::ok("more");
                }
                Response::ok("done")
            }
            Err(e) => {
                console_error!("scorer {user}: {}", e.message);
                self.state.storage().set_alarm(RETRY_MS).await?;
                Response::ok("retry")
            }
        }
    }
}
