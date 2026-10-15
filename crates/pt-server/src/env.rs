//! What the server logic needs from its host. `pt-worker` implements these with
//! Cloudflare bindings (D1, R2, Queues, Durable Objects, fetch); the native
//! tests implement them with rusqlite and memory (tests/common).
//!
//! The futures are not `Send`: Workers run single-threaded.

use serde_json::{Map, Value};

/// A query parameter. D1 binds JavaScript values, so only these four kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum P {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl From<i64> for P {
    fn from(v: i64) -> P {
        P::Int(v)
    }
}
impl From<u64> for P {
    fn from(v: u64) -> P {
        P::Int(v as i64)
    }
}
impl From<u32> for P {
    fn from(v: u32) -> P {
        P::Int(v as i64)
    }
}
impl From<bool> for P {
    fn from(v: bool) -> P {
        P::Int(v as i64)
    }
}
impl From<f64> for P {
    fn from(v: f64) -> P {
        P::Real(v)
    }
}
impl From<&str> for P {
    fn from(v: &str) -> P {
        P::Text(v.to_string())
    }
}
impl From<String> for P {
    fn from(v: String) -> P {
        P::Text(v)
    }
}
impl From<&String> for P {
    fn from(v: &String) -> P {
        P::Text(v.clone())
    }
}
impl<T: Into<P>> From<Option<T>> for P {
    fn from(v: Option<T>) -> P {
        v.map_or(P::Null, Into::into)
    }
}

/// Builds a parameter list: `params![a, b, c]`.
#[macro_export]
macro_rules! params {
    ($($x:expr),* $(,)?) => { vec![$($crate::env::P::from($x)),*] };
}

pub type Row = Map<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type HostResult<T> = Result<T, HostError>;

/// A statement for [`Host::batch`].
pub type Stmt = (String, Vec<P>);

/// Settings from the Worker's variables and secrets.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// SHA-256 (hex) of the read token and of the bot token.
    pub read_token_sha256: Vec<String>,
    pub bot_token_sha256: Vec<String>,
    /// Discord IDs that are admins even before any admin exists (bootstrap).
    pub bootstrap_admins: Vec<String>,
    /// Where webhooks go, and the secrets they are signed with (WEBHOOKS §1-2).
    pub webhook_url: Option<String>,
    pub webhook_secrets: Vec<String>,
    /// This server's public origin, for signed download URLs.
    pub public_url: String,
    /// Key for signed download URLs.
    pub download_secret: String,
    /// Where this server's backups go in the backup bucket, ending in `/`
    /// (docs/SERVER.md).
    pub backup_prefix: String,
    /// Keys whose signed updates may be published; empty means the release
    /// keys built into every client (`pt_core::update::UPDATE_KEYS`).
    pub update_keys: Vec<[u8; 32]>,
}

#[allow(async_fn_in_trait)]
pub trait Host {
    fn settings(&self) -> &Settings;
    fn now_ms(&self) -> i64;
    fn random(&self, buf: &mut [u8]);

    /// Rows as JSON objects (column name → value).
    async fn all(&self, sql: &str, params: Vec<P>) -> HostResult<Vec<Row>>;
    /// Number of rows changed.
    async fn run(&self, sql: &str, params: Vec<P>) -> HostResult<u64>;
    /// Several statements in one transaction.
    async fn batch(&self, stmts: Vec<Stmt>) -> HostResult<()>;

    async fn blob_get(&self, key: &str) -> HostResult<Option<Vec<u8>>>;
    async fn blob_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()>;
    async fn blob_delete(&self, key: &str) -> HostResult<()>;

    /// The backup bucket (a separate R2 bucket): write, list keys under a
    /// prefix, delete.
    async fn backup_put(&self, key: &str, bytes: Vec<u8>) -> HostResult<()>;
    async fn backup_list(&self, prefix: &str) -> HostResult<Vec<String>>;
    async fn backup_delete(&self, key: &str) -> HostResult<()>;

    /// The release bucket (client updates and setup programs, docs/UPDATES.md):
    /// an object's size, and its bytes.
    async fn release_size(&self, key: &str) -> HostResult<Option<u64>>;
    async fn release_get(&self, key: &str) -> HostResult<Option<Vec<u8>>>;

    /// Asks the player's scorer to process new chunks. It must not wait for the
    /// scoring to finish (the Worker hands it to the Durable Object).
    async fn poke_scorer(&self, user_id: &str, game: &str) -> HostResult<()>;
    /// Queues a webhook delivery after `delay_s` seconds.
    async fn queue_webhook(&self, event_id: &str, delay_s: u32) -> HostResult<()>;
    /// Queues one step of a bulk operation.
    async fn queue_op(&self, msg: Value) -> HostResult<()>;
    /// POSTs a webhook; returns the HTTP status (or an error for network failures
    /// and timeouts).
    async fn http_post(&self, url: &str, headers: Vec<(String, String)>, body: Vec<u8>) -> HostResult<u16>;
}

/// Reading typed values out of a [`Row`]. D1 returns every number as a
/// JavaScript number, so integers are read leniently.
pub trait RowExt {
    fn s(&self, k: &str) -> String;
    fn opt_s(&self, k: &str) -> Option<String>;
    fn i(&self, k: &str) -> i64;
    fn opt_i(&self, k: &str) -> Option<i64>;
    fn f(&self, k: &str) -> f64;
    fn opt_f(&self, k: &str) -> Option<f64>;
    fn b(&self, k: &str) -> bool;
    fn json(&self, k: &str) -> Value;
}

impl RowExt for Row {
    fn s(&self, k: &str) -> String {
        self.opt_s(k).unwrap_or_default()
    }
    fn opt_s(&self, k: &str) -> Option<String> {
        match self.get(k) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }
    fn i(&self, k: &str) -> i64 {
        self.opt_i(k).unwrap_or(0)
    }
    fn opt_i(&self, k: &str) -> Option<i64> {
        match self.get(k) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        }
    }
    fn f(&self, k: &str) -> f64 {
        self.opt_f(k).unwrap_or(0.0)
    }
    fn opt_f(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(Value::as_f64)
    }
    fn b(&self, k: &str) -> bool {
        self.i(k) != 0
    }
    fn json(&self, k: &str) -> Value {
        self.opt_s(k).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
    }
}
