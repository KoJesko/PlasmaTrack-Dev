//! Peak memory of a full rebuild, as a Durable Object (128 MB) would hold it.
//! On 2026-10-16 the owner's ETS2 rebuild never finished on Cloudflare: nine
//! jobs of the last 48 h (capture mode included) kept 373k points, and the last
//! catch-up pass copied every job to store it.
//!
//! Skipped unless `PT_MEMORY_FIXTURE` names a recording (JSONL, one frame per
//! line):
//!
//!     PT_MEMORY_FIXTURE=ets2.jsonl cargo test --release -p pt-server --test scorer_memory -- --nocapture

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::BufRead;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::seed::*;
use common::{block_on, TestHost};
use pt_core::Game;
use pt_server::scorer::Scorer;

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

fn blob_bytes(t: &TestHost) -> usize {
    t.blobs.borrow().values().map(Vec::len).sum()
}

#[test]
fn a_full_rebuild_fits_a_durable_object() {
    let Some(path) = std::env::var_os("PT_MEMORY_FIXTURE") else { return };
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    let file = std::fs::File::open(path).unwrap();
    let lines: Vec<String> = std::io::BufReader::new(file).lines().map(Result::unwrap).filter(|l| !l.trim().is_empty()).collect();
    upload(&t, &token, &lines, 80);
    let frames = lines.len();
    drop(lines);

    let game = if t.q("SELECT game FROM plugin_sessions WHERE game IS NOT NULL")[0]["game"] == "ats" { Game::Ats } else { Game::Ets2 };
    let mut s = Scorer::new(&user_id(&t), game);
    let (mut passes, mut stored) = (0, 0);
    let mut worst = 0.0f64;
    let (start, start_blobs) = (CURRENT.load(Ordering::Relaxed), blob_bytes(&t));
    loop {
        // What the scorer holds: everything allocated beyond the host's own storage.
        let (base, blobs) = (CURRENT.load(Ordering::Relaxed), blob_bytes(&t));
        PEAK.store(base, Ordering::Relaxed);
        let r = block_on(s.process(&t)).unwrap();
        let new_blobs = blob_bytes(&t).saturating_sub(blobs);
        // Held between passes (the timelines) plus the pass's own peak.
        let held = (base - start).saturating_sub(blobs - start_blobs);
        let pass = (PEAK.load(Ordering::Relaxed) - base).saturating_sub(new_blobs);
        let peak = (held + pass) as f64 / MB;
        if peak > worst {
            println!("pass {}: held {:.1} MB + pass {:.1} MB ({} chunks, {} jobs stored)", passes + 1, held as f64 / MB, pass as f64 / MB, r.chunks, r.jobs_stored);
        }
        worst = worst.max(peak);
        passes += 1;
        stored += r.jobs_stored;
        if !r.more {
            break;
        }
    }
    println!("{frames} frames, {passes} passes, {stored} jobs stored, peak above the host: {worst:.1} MB");
    assert!(stored > 0);
    assert!(worst < 80.0, "a pass peaked at {worst:.1} MB; a Durable Object has 128 MB in all");
}
