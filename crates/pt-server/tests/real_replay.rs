//! A player's real data, scored as production would: D1 rows and raw chunks
//! copied from the server into the test host, then the scorer's passes, with
//! the memory each one needs measured. Also replays the same frames with every
//! point in memory (as the client does) and compares the jobs.
//!
//! Skipped unless `PT_REAL_DIR` names a directory with `rows_<table>.json`
//! (`wrangler d1 execute --json` output), `keys.txt` (`NNNNN r2_key` per line)
//! and `chunks/NNNNN.gz`:
//!
//!     PT_REAL_DIR=... cargo test --release -p pt-server --test real_replay -- --nocapture

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{block_on, TestHost};
use pt_core::rules::Context;
use pt_core::timeline::Profiles;
use pt_core::trace::parse_frame;
use pt_core::Game;
use pt_replay::sample_config;
use serde_json::Value;

struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let now = CURRENT.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new > l.size() {
                grew(new - l.size());
            } else {
                CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const MB: f64 = 1024.0 * 1024.0;

fn rows(dir: &Path, table: &str) -> Vec<serde_json::Map<String, Value>> {
    let raw = std::fs::read_to_string(dir.join(format!("rows_{table}.json"))).unwrap();
    let v: Value = serde_json::from_str(&raw[raw.find('[').unwrap()..]).unwrap();
    v[0]["results"].as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect()
}

fn insert(t: &TestHost, table: &str, rows: &[serde_json::Map<String, Value>]) {
    for r in rows {
        let cols: Vec<&String> = r.keys().collect();
        let sql = format!(
            "INSERT OR REPLACE INTO {table} ({}) VALUES ({})",
            cols.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "),
            (1..=cols.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
        );
        let vals: Vec<rusqlite::types::Value> = cols
            .iter()
            .map(|c| match &r[*c] {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) if n.is_i64() => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
                Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap()),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                other => rusqlite::types::Value::Text(other.to_string()),
            })
            .collect();
        t.db.execute(&sql, rusqlite::params_from_iter(vals)).unwrap_or_else(|e| panic!("{table}: {e}"));
    }
}

fn blob_bytes(t: &TestHost) -> usize {
    t.blobs.borrow().values().map(Vec::len).sum()
}

#[test]
fn a_players_real_data_scores_within_a_durable_objects_memory() {
    let Some(dir) = std::env::var_os("PT_REAL_DIR") else { return };
    let dir = Path::new(&dir);
    let t = TestHost::new();
    t.db.execute("DELETE FROM config_versions", []).unwrap();
    for table in ["users", "config_versions", "releases", "revocations", "bonuses", "plugin_sessions", "raw_chunks"] {
        insert(&t, table, &rows(dir, table));
    }
    let user = rows(dir, "users")[0]["user_id"].as_str().unwrap().to_string();
    // Chunks, in the order they were listed (session start, then chunk number).
    let keys = std::fs::read_to_string(dir.join("keys.txt")).unwrap();
    let mut order: Vec<String> = Vec::new();
    for line in keys.lines().filter(|l| !l.is_empty()) {
        let (n, key) = line.split_once(' ').unwrap();
        let data = std::fs::read(dir.join("chunks").join(format!("{n}.gz"))).unwrap_or_else(|_| panic!("chunk {n} missing"));
        t.blobs.borrow_mut().insert(key.to_string(), data);
        order.push(key.to_string());
    }
    t.now.set(1_792_468_000_000);

    let mut s = t.scorer(&user, Game::Ats);
    let (mut passes, mut chunks, mut stored, mut worst) = (0, 0, 0, 0.0f64);
    let start = CURRENT.load(Ordering::Relaxed);
    let start_blobs = blob_bytes(&t);
    loop {
        let (base, blobs) = (CURRENT.load(Ordering::Relaxed), blob_bytes(&t));
        PEAK.store(base, Ordering::Relaxed);
        let r = block_on(s.process(&t)).unwrap();
        let new_blobs = blob_bytes(&t).saturating_sub(blobs);
        let held = (base - start).saturating_sub(blobs - start_blobs);
        let pass = (PEAK.load(Ordering::Relaxed) - base).saturating_sub(new_blobs);
        let peak = (held + pass) as f64 / MB;
        let (in_mem, on_disk) = s.footprint();
        println!(
            "pass {:>2}: {:>3} chunks, {} stored, held {:.1} MB + pass {:.1} MB; {in_mem} points in memory, {:.1} MB of points on disk",
            passes + 1,
            r.chunks,
            r.jobs_stored,
            held as f64 / MB,
            pass as f64 / MB,
            on_disk as f64 / MB
        );
        worst = worst.max(peak);
        passes += 1;
        chunks += r.chunks;
        stored += r.jobs_stored;
        if !r.more {
            break;
        }
    }
    println!("{chunks} chunks in {passes} passes, {stored} job stores; worst pass {worst:.1} MB above the host");
    for j in t.q("SELECT local_id, state, start_kind, outcome, points, held_points, world_km, game_km, reasons FROM jobs ORDER BY local_id") {
        println!("job {}", j);
    }

    // The same frames with every point in memory, as the client scores them.
    let cfg = sample_config();
    let names = cfg.multiplayer.truckersmp_module_names.clone();
    let mut client = Profiles::new(cfg.clone());
    for key in &order {
        let text = pt_server::util::gunzip(&t.blobs.borrow()[key], usize::MAX).unwrap();
        for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            if let Ok(f) = parse_frame(std::str::from_utf8(line).unwrap(), &names) {
                client.push(&f);
            }
        }
    }
    for j in client.jobs() {
        let r = client.evaluate(j, &cfg, &Context::default());
        let (kept, reverted) = j.point_counts(client.store());
        println!(
            "client: job {} {:?} {:?} kept {kept} reverted {reverted}: {:?} {:.1} points, {:.1} world km, {:.0} game km",
            j.id, j.state, j.start, r.outcome, r.points_raw, r.distances.world_km, r.distances.counted_game_km
        );
    }
    assert!(worst < 64.0, "a pass needed {worst:.1} MB; a Durable Object has 128 MB in all");
}
