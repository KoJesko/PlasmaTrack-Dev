//! A native [`Host`] for the tests: SQLite in memory with the D1 migrations,
//! R2 as a map, a settable clock, a deterministic RNG, and recorded queue
//! messages, scorer pokes and webhook POSTs.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use pt_server::env::{Host, HostError, HostResult, Row, Settings, Stmt, P};
use pt_server::http::{Request, Response};
use pt_server::util;
use rusqlite::types::ValueRef;
use serde_json::{json, Value};

pub const READ_TOKEN: &str = "read-token-for-tests";
pub const BOT_TOKEN: &str = "bot-token-for-tests";
pub const ADMIN: &str = "900000000000000001";
pub const WEBHOOK_SECRET: &str = "whsec-test";

type Post = (String, Vec<(String, String)>, Vec<u8>);

pub struct TestHost {
    pub db: rusqlite::Connection,
    pub blobs: RefCell<BTreeMap<String, Vec<u8>>>,
    /// The backup bucket.
    pub backups: RefCell<BTreeMap<String, Vec<u8>>>,
    /// The release bucket.
    pub releases: RefCell<BTreeMap<String, Vec<u8>>>,
    pub now: Cell<i64>,
    rng: Cell<u64>,
    pub settings: Settings,
    pub pokes: RefCell<Vec<(String, String)>>,
    pub webhooks_queued: RefCell<Vec<(String, u32)>>,
    pub ops_queued: RefCell<Vec<Value>>,
    pub posts: RefCell<Vec<Post>>,
    pub post_status: Cell<u16>,
}

fn bind(p: &P) -> rusqlite::types::Value {
    match p {
        P::Null => rusqlite::types::Value::Null,
        P::Int(i) => rusqlite::types::Value::Integer(*i),
        P::Real(f) => rusqlite::types::Value::Real(*f),
        P::Text(s) => rusqlite::types::Value::Text(s.clone()),
    }
}

fn err(e: rusqlite::Error) -> HostError {
    HostError(e.to_string())
}

impl TestHost {
    pub fn new() -> TestHost {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("../../migrations/0001_init.sql")).unwrap();
        db.execute_batch(include_str!("../../migrations/0002_manual_jobs.sql")).unwrap();
        db.execute_batch(include_str!("../../migrations/0003_adjusted_sections.sql")).unwrap();
        db.execute_batch(include_str!("../../migrations/0004_client_updates.sql")).unwrap();
        TestHost {
            db,
            blobs: RefCell::new(BTreeMap::new()),
            backups: RefCell::new(BTreeMap::new()),
            releases: RefCell::new(BTreeMap::new()),
            now: Cell::new(util::parse_rfc3339("2026-10-14T09:00:00Z").unwrap()),
            rng: Cell::new(0x9E37_79B9_7F4A_7C15),
            settings: Settings {
                read_token_sha256: vec![util::sha256_hex(READ_TOKEN.as_bytes())],
                bot_token_sha256: vec![util::sha256_hex(BOT_TOKEN.as_bytes())],
                bootstrap_admins: vec![ADMIN.to_string()],
                webhook_url: Some("https://bot.example/webhooks/plasmatrack".into()),
                webhook_secrets: vec![WEBHOOK_SECRET.into()],
                public_url: "https://pt.example".into(),
                download_secret: "dl-secret".into(),
                backup_prefix: "d1/test-server/".into(),
                update_keys: vec![update_signing_key().verifying_key().to_bytes()],
            },
            pokes: RefCell::new(Vec::new()),
            webhooks_queued: RefCell::new(Vec::new()),
            ops_queued: RefCell::new(Vec::new()),
            posts: RefCell::new(Vec::new()),
            post_status: Cell::new(202),
        }
    }

    pub fn advance(&self, ms: i64) {
        self.now.set(self.now.get() + ms);
    }

    /// Runs a request through the server.
    pub fn call(&self, req: Request) -> Response {
        block_on(pt_server::handle(self, req))
    }

    /// A bot-token request.
    pub fn bot(&self, method: &str, path: &str) -> Request {
        Request::new(method, path).with_header("authorization", &format!("Bearer {BOT_TOKEN}"))
    }

    /// An admin request by [`ADMIN`], with an idempotency key on writes.
    pub fn admin(&self, method: &str, path: &str) -> Request {
        let r = self.bot(method, path).with_header("x-plasmatrack-actor", ADMIN);
        if method == "GET" {
            r
        } else {
            r.with_header("idempotency-key", &format!("k-{}-{}", path, self.now.get()))
        }
    }

/// Runs a whole SQL script (a backup restore).
    pub fn exec(&self, sql: &str) {
        self.db.execute_batch(sql).unwrap();
    }

    pub fn q(&self, sql: &str) -> Vec<Value> {
        let mut st = self.db.prepare(sql).unwrap();
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let rows = st
            .query_map([], |r| {
                let mut o = serde_json::Map::new();
                for (i, n) in names.iter().enumerate() {
                    o.insert(n.clone(), value(r.get_ref(i)?));
                }
                Ok(Value::Object(o))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }
}

fn value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => Value::Null,
    }
}

pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

impl Host for TestHost {
    fn settings(&self) -> &Settings {
        &self.settings
    }
    fn now_ms(&self) -> i64 {
        self.now.get()
    }
    fn random(&self, buf: &mut [u8]) {
        for b in buf {
            // xorshift64*
            let mut x = self.rng.get();
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.rng.set(x);
            *b = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }

    async fn all(&self, sql: &str, params: Vec<P>) -> HostResult<Vec<Row>> {
        let mut st = self.db.prepare(sql).map_err(err)?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let vals: Vec<rusqlite::types::Value> = params.iter().map(bind).collect();
        let rows = st
            .query_map(rusqlite::params_from_iter(vals), |r| {
                let mut o = Row::new();
                for (i, n) in names.iter().enumerate() {
                    o.insert(n.clone(), value(r.get_ref(i)?));
                }
                Ok(o)
            })
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    async fn run(&self, sql: &str, params: Vec<P>) -> HostResult<u64> {
        let vals: Vec<rusqlite::types::Value> = params.iter().map(bind).collect();
        self.db.execute(sql, rusqlite::params_from_iter(vals)).map(|n| n as u64).map_err(err)
    }

    async fn batch(&self, stmts: Vec<Stmt>) -> HostResult<()> {
        self.db.execute_batch("BEGIN").map_err(err)?;
        for (sql, params) in &stmts {
            let vals: Vec<rusqlite::types::Value> = params.iter().map(bind).collect();
            if let Err(e) = self.db.execute(sql, rusqlite::params_from_iter(vals)) {
                self.db.execute_batch("ROLLBACK").map_err(err)?;
                return Err(err(e));
            }
        }
        self.db.execute_batch("COMMIT").map_err(err)
    }

    async fn blob_get(&self, key: &str) -> HostResult<Option<Vec<u8>>> {
        Ok(self.blobs.borrow().get(key).cloned())
    }
    async fn blob_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()> {
        self.blobs.borrow_mut().insert(key.to_string(), bytes);
        Ok(())
    }
    async fn blob_delete(&self, key: &str) -> HostResult<()> {
        self.blobs.borrow_mut().remove(key);
        Ok(())
    }

    async fn backup_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()> {
        self.backups.borrow_mut().insert(key.to_string(), bytes);
        Ok(())
    }

    async fn backup_list(&self, prefix: &str) -> HostResult<Vec<String>> {
        Ok(self.backups.borrow().keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }

    async fn backup_delete(&self, key: &str) -> HostResult<()> {
        self.backups.borrow_mut().remove(key);
        Ok(())
    }

    async fn release_size(&self, key: &str) -> HostResult<Option<u64>> {
        Ok(self.releases.borrow().get(key).map(|b| b.len() as u64))
    }

    async fn release_get(&self, key: &str) -> HostResult<Option<Vec<u8>>> {
        Ok(self.releases.borrow().get(key).cloned())
    }

    async fn poke_scorer(&self, user_id: &str, game: &str) -> HostResult<()> {
        self.pokes.borrow_mut().push((user_id.to_string(), game.to_string()));
        Ok(())
    }
    async fn queue_webhook(&self, event_id: &str, delay_s: u32) -> HostResult<()> {
        self.webhooks_queued.borrow_mut().push((event_id.to_string(), delay_s));
        Ok(())
    }
    async fn queue_op(&self, msg: Value) -> HostResult<()> {
        self.ops_queued.borrow_mut().push(msg);
        Ok(())
    }
    async fn http_post(&self, url: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> HostResult<u16> {
        self.posts.borrow_mut().push((url.to_string(), headers, body));
        Ok(self.post_status.get())
    }
}
pub mod seed;

/// Signs test updates; the test host accepts its public key.
pub fn update_signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[9; 32])
}

/// A signed client update for `version`: its files (gzip-compressed) and the
/// setup program in the release bucket, and the release's `files` hashes.
pub struct SignedUpdate {
    pub manifest: String,
    pub signature: String,
    pub files: Vec<Value>,
}

pub fn signed_update(t: &TestHost, version: &str) -> SignedUpdate {
    use ed25519_dalek::Signer;
    use pt_core::update::{UpdateFile, UpdateManifest, PRODUCT, SIGNING_CONTEXT};
    use std::io::Write;
    let gzip = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let contents = [
        ("plasmatrack-client.exe", format!("client {version}")),
        ("plasmatrack-clientw.exe", format!("clientw {version}")),
        ("plasmatrack.dll", format!("dll {version}")),
        ("licenses/LLVM-LICENSE.txt", "licence".to_string()),
    ];
    let mut m = UpdateManifest { product: PRODUCT.into(), platform: "windows".into(), version: version.into(), files: vec![] };
    for (path, body) in &contents {
        let gz = gzip(body.as_bytes());
        let f = UpdateFile { path: path.to_string(), sha256: util::sha256_hex(body.as_bytes()), size: body.len() as u64, gz_size: gz.len() as u64 };
        t.releases.borrow_mut().insert(m.key(&f), gz);
        m.files.push(f);
    }
    t.releases.borrow_mut().insert(format!("windows/{version}/PlasmaTrack-Setup-{version}.exe"), b"setup".to_vec());
    let manifest = serde_json::to_string(&m).unwrap();
    let sig = update_signing_key().sign(&[SIGNING_CONTEXT, manifest.as_bytes()].concat());
    let signature = sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let sha = |p: &str| m.files.iter().find(|f| f.path == p).unwrap().sha256.clone();
    let files = vec![
        json!({"role": "plugin", "path_pattern": "*/plugins/plasmatrack.dll", "sha256": sha("plasmatrack.dll")}),
        json!({"role": "client_binary", "path_pattern": "*/plasmatrack-clientw.exe", "sha256": sha("plasmatrack-clientw.exe")}),
        json!({"role": "client_binary", "path_pattern": "*/plasmatrack-client.exe", "sha256": sha("plasmatrack-client.exe")}),
    ];
    SignedUpdate { manifest, signature, files }
}
