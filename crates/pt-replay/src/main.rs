//! `pt-replay [--config FILE] [--json] [--sp-division] CAPTURE...`
//!
//! Replays captures or fixtures and prints every job with its score.

use std::path::PathBuf;

use pt_core::rules::Context;
use pt_core::units::km_to_mi;
use pt_replay::{order_sessions, parse_lines, read_lines, replay, sample_config};

fn main() {
    let mut cfg = sample_config();
    let mut json = false;
    let mut ctx = Context::default();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut trim_to: Option<PathBuf> = None;
    let mut every = 15usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => {
                let p = args.next().expect("--config needs a file");
                let text = std::fs::read_to_string(&p).expect("readable config");
                cfg = pt_core::Config::from_toml_str(&text).expect("valid config");
            }
            "--json" => json = true,
            "--sp-division" => ctx.singleplayer_division = true,
            "--trim-to" => trim_to = Some(PathBuf::from(args.next().expect("--trim-to needs a file"))),
            "--every" => every = args.next().expect("--every needs N").parse().expect("number"),
            _ => files.push(a.into()),
        }
    }
    if files.is_empty() {
        eprintln!("usage: pt-replay [--config FILE] [--json] [--sp-division] [--trim-to OUT.jsonl.gz [--every N]] CAPTURE.jsonl[.gz]...");
        std::process::exit(2);
    }
    let mut lines = Vec::new();
    for f in &files {
        lines.extend(read_lines(f).unwrap_or_else(|e| panic!("{}: {e}", f.display())));
    }
    if let Some(out) = trim_to {
        let trimmed = pt_replay::trim(&lines, every, &cfg.multiplayer);
        pt_replay::write_gz(&out, &trimmed).expect("write fixture");
        eprintln!("wrote {} ({} of {} lines)", out.display(), trimmed.len(), lines.len());
        return;
    }
    let (mut frames, bad) = parse_lines(&lines, &cfg);
    order_sessions(&mut frames);
    if bad > 0 {
        eprintln!("skipped {bad} unparsable line(s) (e.g. a capture cut off by a crash)");
    }
    let jobs = replay(&frames, &cfg, &ctx);
    if json {
        let out: Vec<_> = jobs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.job.id, "profile": r.job.profile, "state": r.job.state, "start": r.job.start,
                    "info": r.job.info, "events": r.job.events, "gaps": r.job.gaps,
                    "kept_points": r.job.kept.len(),
                    "reverted_points": r.job.reverted.iter().map(|b| b.points.len()).sum::<usize>(),
                    "delivered": r.job.delivered, "result": r.result,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out).expect("serializable"));
        return;
    }
    for r in &jobs {
        let i = &r.job.info;
        let rt = &i.fingerprint.route;
        let d = &r.result.distances;
        println!(
            "job {} [{}] {} {}/{} -> {}/{} (planned {} km, {}) state={:?} start={:?}",
            r.job.id,
            r.job.profile,
            i.fingerprint.cargo.cargo_id,
            rt.source_city,
            rt.source_company,
            rt.destination_city,
            rt.destination_company,
            rt.planned_distance_km.map_or("?".into(), |v| v.to_string()),
            rt.job_market,
            r.job.state,
            r.job.start
        );
        println!(
            "  tracked: whole {:.2} game km = counted {:.2} + uncounted {:.2}; counted 1:1 {:.3} km; kept {} pts, reverted {} pts, gaps {}",
            d.tracked_whole_game_km,
            d.tracked_counted_game_km,
            d.tracked_uncounted_game_km,
            d.tracked_counted_world_km,
            r.job.kept.len(),
            r.job.reverted.iter().map(|b| b.points.len()).sum::<usize>(),
            r.job.gaps.len()
        );
        println!(
            "  game delivered {:?} km -> counted {:.2} game km ({:.1} mi in game) -> {:.3} km 1:1 = {:.3} mi",
            d.delivered_game_km,
            d.counted_game_km,
            km_to_mi(d.counted_game_km),
            d.world_km,
            d.world_mi
        );
        println!(
            "  speed: peak {:.1} mph, over penalty {:.1}s, over invalid {:.1}s; refuels {}; truckersmp={} convoy={} mod={}",
            r.result.speed.peak_mph,
            r.result.speed.over_penalty_s,
            r.result.speed.over_invalid_s,
            r.result.refuels.len(),
            r.result.truckersmp,
            r.result.convoy,
            r.result.multiplayer_mod.as_deref().unwrap_or("-")
        );
        println!(
            "  => {:?}{} points {:.1} (raw {:.3}); review {:?}",
            r.result.outcome,
            r.result.discard.as_ref().map_or(String::new(), |d| format!(" ({d:?})")),
            r.result.points,
            r.result.points_raw,
            r.result.review
        );
        for e in &r.job.events {
            println!("    {}", serde_json::to_string(e).expect("serializable"));
        }
    }
}
