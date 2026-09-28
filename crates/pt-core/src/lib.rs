//! PlasmaTrack shared core.
//!
//! The client (provisional results) and the server (authoritative results) run
//! exactly this code over the same raw telemetry stream, so a job scores the same
//! on both sides. Nothing here performs I/O.

pub mod config;
pub mod game;
pub mod job;
pub mod rules;
pub mod timeline;
pub mod trace;
pub mod units;
pub mod upload;
pub mod wire;

pub use config::Config;
pub use game::Game;
