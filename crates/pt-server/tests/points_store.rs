//! The scorer's SQLite point store gives exactly what the in-memory one does:
//! every fixture and local capture replayed through both, jobs and scores
//! compared at checkpoints; and random sequences of the store's operations
//! across block boundaries.

mod common;

use std::path::{Path, PathBuf};

use common::TestSql;
use pt_core::rules::Context;
use pt_core::store::{PointStore, TrackId, VecStore};
use pt_core::timeline::{Point, Profiles};
use pt_core::trace::Frame;
use pt_replay::{order_sessions, parse_lines, read_lines, sample_config};
use pt_server::points::{SqlPoints, BLOCK};

const CHECKPOINT_EVERY: usize = 2_500;

fn replay_both(frames: &[Frame]) {
    let cfg = sample_config();
    let ctx = Context::default();
    let mut mem = Profiles::new(cfg.clone());
    let mut sql = Profiles::with_store(cfg.clone(), SqlPoints::open(TestSql::new()));
    let check = |mem: &Profiles, sql: &Profiles<SqlPoints<TestSql>>, at: &str| {
        let a: Vec<_> = mem.jobs().map(|j| (mem.materialize(j), mem.evaluate(j, &cfg, &ctx))).collect();
        let b: Vec<_> = sql.jobs().map(|j| (sql.materialize(j), sql.evaluate(j, &cfg, &ctx))).collect();
        assert_eq!(a.len(), b.len(), "{at}: job count");
        for (x, y) in a.iter().zip(&b) {
            assert!(x == y, "{at}: job {}/{} differs between the stores", x.0.profile, x.0.id);
        }
        assert!(sql.store().points_in_memory() <= 4 * BLOCK, "{at}: {} points in memory", sql.store().points_in_memory());
    };
    for (i, f) in frames.iter().enumerate() {
        mem.push(f);
        sql.push(f);
        if (i + 1) % CHECKPOINT_EVERY == 0 {
            // A flush in the middle, as after every chunk on the server.
            sql.store_mut().flush();
            check(&mem, &sql, &format!("frame {}", i + 1));
        }
    }
    check(&mem, &sql, "end");
    let ev_a = mem.take_evicted();
    let ev_b = sql.take_evicted();
    assert_eq!(ev_a, ev_b);
    assert_eq!(mem.into_jobs(), sql.into_jobs());
}

fn frames_of(files: &[PathBuf]) -> Vec<Frame> {
    let cfg = sample_config();
    let mut frames = Vec::new();
    for f in files {
        frames.extend(parse_lines(&read_lines(f).unwrap(), &cfg).0);
    }
    order_sessions(&mut frames);
    frames
}

#[test]
fn fixtures_replay_the_same_from_sqlite() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pt-replay/tests/fixtures/real");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    for f in files {
        replay_both(&frames_of(&[f]));
    }
}

/// The full-rate captures (git-ignored), when present. Slow in a debug build:
/// `cargo test --release -p pt-server --test points_store`.
#[test]
fn captures_replay_the_same_from_sqlite() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let mut files: Vec<PathBuf> =
            std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
        files.sort();
        if !files.is_empty() {
            replay_both(&frames_of(&files));
        }
    }
}

fn pt(i: u64) -> Point {
    Point {
        sess: (i % 3) as u32,
        seq: i,
        wall_ms: i as i64 * 50,
        sim_us: i * 50_000,
        run: (i / 700) as u32,
        speed_mps: (i % 31) as f64,
        odometer_km: Some(i as f64 * 0.02),
        fuel_l: if i % 5 == 0 { None } else { Some(400.0 - i as f64 * 0.001) },
        pos: if i % 11 == 0 { None } else { Some([(i % 997) as f64 * 3.0, 1.0, (i / 997) as f64 * 5.0]) },
        game_time_min: Some(i as u32 / 20),
        local_scale: 19.0,
        cargo_loaded: i % 2 == 0,
        trailer_connected: true,
        truck: (i / 5000) as u32,
        mp_time_offset: 0,
        truckersmp: false,
        after_teleport: i % 1000 == 0,
        cargo_damage: None,
    }
}

/// A small deterministic generator (no extra crates).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

#[test]
fn random_operations_match_the_memory_store() {
    let mut rng = Lcg(42);
    let mut a = VecStore::new();
    let db = TestSql::new();
    let mut b = SqlPoints::open(db.clone());
    // Pairs of the same track in both stores.
    let mut tracks: Vec<(TrackId, TrackId)> = vec![(a.create(), b.create())];
    let mut seq = 0u64;
    for step in 0..3000 {
        let k = rng.next(tracks.len());
        match rng.next(20) {
            0..=11 => {
                for _ in 0..rng.next(3 * BLOCK) {
                    seq += 1;
                    a.push(tracks[k].0, pt(seq));
                    b.push(tracks[k].1, pt(seq));
                }
            }
            12..=13 => {
                let at = rng.next(a.len(tracks[k].0) + 1);
                tracks.push((a.split_off(tracks[k].0, at), b.split_off(tracks[k].1, at)));
            }
            14 if tracks.len() > 1 => {
                let j = (k + 1 + rng.next(tracks.len() - 1)) % tracks.len();
                let (src_a, src_b) = tracks.remove(j);
                let k = k.min(tracks.len() - 1);
                a.append(tracks[k].0, src_a);
                b.append(tracks[k].1, src_b);
            }
            15 if tracks.len() > 1 => {
                let (x, y) = tracks.remove(k);
                a.remove(x);
                b.remove(y);
            }
            16 => b.flush(),
            17 => {
                // A restart: the store is read back from the database.
                b.flush();
                let mut fresh = SqlPoints::open(db.clone());
                std::mem::swap(&mut b, &mut fresh);
            }
            _ => {
                let (x, y) = tracks[k];
                let n = a.len(x);
                assert_eq!(n, b.len(y), "step {step}: length");
                if n > 0 {
                    let i = rng.next(n);
                    assert_eq!(a.get(x, i), b.get(y, i), "step {step}: point {i}");
                    let p = a.get(x, i).unwrap();
                    if let Some(pos) = p.pos {
                        let near_a: Vec<usize> = a.iter(x).enumerate().filter(|(_, q)| q.pos.is_some_and(|q| pt_core::timeline::dist3(q, pos) <= 50.0)).map(|(i, _)| i).collect();
                        let near_b: Vec<usize> = b.near(y, pos, 50.0).filter(|(_, q)| q.pos.is_some_and(|q| pt_core::timeline::dist3(q, pos) <= 50.0)).map(|(i, _)| i).collect();
                        assert_eq!(near_a, near_b, "step {step}: near");
                    }
                }
            }
        }
    }
    b.flush();
    for (x, y) in &tracks {
        assert_eq!(a.to_vec(*x), b.to_vec(*y));
    }
    let total: usize = tracks.iter().map(|(x, _)| a.len(*x)).sum();
    assert!(total > 10 * BLOCK, "the test moved enough points: {total}");
}
