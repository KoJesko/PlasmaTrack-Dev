//! Replay harness: raw frames in, jobs and scores out, through exactly the code
//! the client and server use (`pt-core`).
//!
//! Inputs are JSON Lines of plugin frames (a capture file, or a fixture), plain
//! or gzip-compressed. Several files are merged in session order.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use pt_core::rules::{evaluate, Context, JobResult};
use pt_core::timeline::{Job, Profiles};
use pt_core::trace::{parse_frame, Frame};
use pt_core::Config;

pub fn read_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let reader: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut out = Vec::new();
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(line);
        }
    }
    Ok(out)
}

/// Parses lines into frames. A truncated last line (a capture cut off by a
/// crash) is skipped and counted.
pub fn parse_lines(lines: &[String], cfg: &Config) -> (Vec<Frame>, usize) {
    let mp = &cfg.multiplayer;
    let mut frames = Vec::with_capacity(lines.len());
    let mut bad = 0;
    for l in lines {
        match parse_frame(l, mp) {
            Ok(f) => frames.push(f),
            Err(_) => bad += 1,
        }
    }
    (frames, bad)
}

/// Orders frames of several sessions: sessions by their first wall-clock time,
/// frames within a session by seq (the plugin's order).
pub fn order_sessions(frames: &mut [Frame]) {
    let mut first: Vec<(String, i64)> = Vec::new();
    for f in frames.iter() {
        match first.iter_mut().find(|(s, _)| *s == f.sess) {
            Some((_, w)) => {
                if f.wall_ms > 0 && (*w == 0 || f.wall_ms < *w) {
                    *w = f.wall_ms;
                }
            }
            None => first.push((f.sess.clone(), f.wall_ms)),
        }
    }
    first.sort_by_key(|(_, w)| *w);
    let rank: Vec<String> = first.iter().map(|(s, _)| s.clone()).collect();
    frames.sort_by_key(|f| (rank.iter().position(|s| *s == f.sess).unwrap_or(usize::MAX), f.seq));
}

pub struct Replayed {
    pub job: Job,
    pub result: JobResult,
}

pub fn replay(frames: &[Frame], cfg: &Config, ctx: &Context) -> Vec<Replayed> {
    let mut profiles = Profiles::new(cfg.clone());
    for f in frames {
        profiles.push(f);
    }
    profiles
        .into_jobs()
        .into_iter()
        .map(|job| {
            let result = evaluate(&job, cfg, ctx);
            Replayed { job, result }
        })
        .collect()
}

pub fn sample_config() -> Config {
    Config::from_toml_str(include_str!("../../../config/plasmatrack.sample.toml")).expect("sample config is valid")
}

/// Shrinks a capture into a committable regression fixture:
/// - samples decimated to every `every`-th one, but the samples on both sides of
///   any other message (a bracket) are always kept, so loads, deliveries and
///   teleports replay exactly;
/// - `seq` renumbered per session so the decimation does not look like data gaps;
/// - `modules` redacted the way the client does before upload;
/// - heartbeats dropped.
///
/// Distances survive exactly (odometer differences telescope).
pub fn trim(lines: &[String], every: usize, mp: &pt_core::config::Multiplayer) -> Vec<String> {
    let every = every.max(1);
    // Heartbeats carry nothing the timeline uses; dropping them keeps fixtures small.
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["kind"] != "heartbeat")
        .collect();
    let is_sample = |v: &serde_json::Value| v["kind"] == "sample";
    let n = parsed.len();
    let mut keep = vec![false; n];
    let mut sample_idx = 0usize;
    for i in 0..n {
        if !is_sample(&parsed[i]) {
            keep[i] = true;
            if let Some(j) = (0..i).rev().find(|&j| is_sample(&parsed[j])) {
                keep[j] = true;
            }
            if let Some(j) = (i + 1..n).find(|&j| is_sample(&parsed[j])) {
                keep[j] = true;
            }
        } else {
            if sample_idx % every == 0 {
                keep[i] = true;
            }
            sample_idx += 1;
        }
    }
    if let Some(j) = (0..n).rev().find(|&j| is_sample(&parsed[j])) {
        keep[j] = true;
    }
    let mut next_seq: Vec<(String, u64)> = Vec::new();
    let mut out = Vec::new();
    for (i, mut v) in parsed.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let sess = v["sess"].as_str().unwrap_or_default().to_string();
        if v.get("seq").is_some_and(|s| s.is_u64()) {
            let slot = match next_seq.iter().position(|(s, _)| *s == sess) {
                Some(p) => p,
                None => {
                    next_seq.push((sess, 1));
                    next_seq.len() - 1
                }
            };
            v["seq"] = next_seq[slot].1.into();
            next_seq[slot].1 += 1;
        }
        out.push(pt_core::wire::redact_modules(&v.to_string(), mp));
    }
    out
}

pub fn write_gz(path: &Path, lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let file = std::fs::File::create(path)?;
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    for l in lines {
        gz.write_all(l.as_bytes())?;
        gz.write_all(b"\n")?;
    }
    gz.finish()?;
    Ok(())
}
