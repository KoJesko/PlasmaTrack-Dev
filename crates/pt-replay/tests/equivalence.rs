//! Equivalence guard for changes to how the timeline stores its points
//! (scorer memory, 2026-10-20). Every fixture, and every local capture in
//! `captures/` when present, is replayed through `Profiles`; at checkpoints
//! along the way and at the end, the live jobs (with all their points) and
//! their scores are hashed. The hashes were recorded from the code before the
//! change (`tests/golden/`), so any difference in a job, a point or a score
//! fails here.
//!
//! `PT_UPDATE_GOLDEN=1 cargo test -p pt-replay --test equivalence` rewrites
//! the golden files; only do that for an intended change in results.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use pt_core::rules::{evaluate, Context, JobResult};
use pt_core::timeline::{Job, Profiles};
use pt_core::trace::Frame;
use pt_replay::{order_sessions, parse_lines, read_lines, sample_config};
use sha2::{Digest, Sha256};

/// Frames between checkpoints: enough to land inside loads, gaps and
/// deliveries of the longer captures, few enough to keep the test quick.
const CHECKPOINT_EVERY: usize = 5_000;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn sha(v: &impl serde::Serialize) -> String {
    let bytes = serde_json::to_vec(v).expect("serializable");
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The live jobs with all their points, the way a caller sees them, and
/// their scores taken straight from the point store.
fn live_jobs(p: &Profiles) -> (Vec<Job>, Vec<JobResult>) {
    let cfg = sample_config();
    let ctx = Context::default();
    p.jobs().map(|j| (p.materialize(j), p.evaluate(j, &cfg, &ctx))).unzip()
}

fn job_line(out: &mut String, tag: &str, (jobs, live_results): (Vec<Job>, Vec<JobResult>)) {
    let cfg = sample_config();
    let ctx = Context::default();
    let results: Vec<_> = jobs.iter().map(|j| evaluate(j, &cfg, &ctx)).collect();
    assert_eq!(results, live_results, "{tag}: scoring from the store differs from scoring in memory");
    writeln!(out, "{tag} jobs={} jobs_sha={} results_sha={}", jobs.len(), sha(&jobs), sha(&results)).unwrap();
}

/// One line per checkpoint and per final job: what the golden file holds.
fn digest(frames: &[Frame]) -> String {
    let cfg = sample_config();
    let ctx = Context::default();
    let mut out = String::new();
    let mut profiles = Profiles::new(cfg.clone());
    for (i, f) in frames.iter().enumerate() {
        profiles.push(f);
        if (i + 1) % CHECKPOINT_EVERY == 0 {
            job_line(&mut out, &format!("frame {}", i + 1), live_jobs(&profiles));
        }
    }
    job_line(&mut out, "live", live_jobs(&profiles));
    let jobs = profiles.into_jobs();
    let results = jobs.iter().map(|j| evaluate(j, &cfg, &ctx)).collect();
    job_line(&mut out, "final", (jobs.clone(), results));
    for j in &jobs {
        let r = evaluate(j, &cfg, &ctx);
        let reverted: usize = j.reverted.iter().map(|b| b.points.len()).sum();
        writeln!(
            out,
            "job {}/{} state={:?} start={:?} kept={} reverted={} branches={} gaps={} events={} outcome={:?} points={} job_sha={}",
            j.profile,
            j.id,
            j.state,
            j.start,
            j.kept.len(),
            reverted,
            j.reverted.len(),
            j.gaps.len(),
            j.events.len(),
            r.outcome,
            r.points,
            sha(j),
        )
        .unwrap();
    }
    out
}

fn frames_of(files: &[PathBuf]) -> Vec<Frame> {
    let cfg = sample_config();
    let mut frames = Vec::new();
    for f in files {
        let (mut fr, _bad) = parse_lines(&read_lines(f).expect("readable input"), &cfg);
        frames.append(&mut fr);
    }
    order_sessions(&mut frames);
    frames
}

fn check(name: &str, files: &[PathBuf]) {
    let got = digest(&frames_of(files));
    let golden = root().join("tests/golden").join(format!("{name}.txt"));
    if std::env::var_os("PT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&golden)
        .unwrap_or_else(|_| panic!("no golden file {}: run with PT_UPDATE_GOLDEN=1 on the old code", golden.display()));
    if got != want {
        let first = got.lines().zip(want.lines()).position(|(a, b)| a != b);
        panic!(
            "{name}: results differ from the golden file{}\n--- want\n{want}\n--- got\n{got}",
            first.map(|i| format!(" (first at line {})", i + 1)).unwrap_or_default()
        );
    }
}

#[test]
fn fixtures_match_golden() {
    let dir = root().join("tests/fixtures/real");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    assert!(!names.is_empty());
    for path in names {
        let name = path.file_name().unwrap().to_string_lossy().trim_end_matches(".jsonl.gz").to_string();
        check(&format!("fixture-{name}"), &[path]);
    }
}

/// The full-rate captures (git-ignored, so only where they exist): 20 Hz data
/// with the real number of points per job.
#[test]
fn captures_match_golden() {
    let dir = root().join("../../captures");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no captures/ here; skipped");
        return;
    };
    let mut dirs: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        files.sort();
        if files.is_empty() {
            continue;
        }
        let name = d.file_name().unwrap().to_string_lossy().to_string();
        check(&format!("capture-{name}"), &files);
    }
}
