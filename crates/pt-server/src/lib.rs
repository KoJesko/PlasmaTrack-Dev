//! PlasmaTrack server logic (PLAN §7a, §8). Host-independent: `pt-worker` runs it
//! on Cloudflare Workers; the tests run it natively against SQLite.

pub mod env;
pub mod http;
pub mod util;
pub mod auth;
pub mod ingest;
pub mod store;
pub mod config;
pub mod jobs;
pub mod webhooks;
pub mod scorer;
pub mod admin;
pub mod manage;
pub mod public;
pub mod route;
pub mod router;

pub use router::handle;
