//! Regression tests on real game captures (ATS 1.61.2.0s, 2026-09-28), trimmed
//! with `pt-replay --trim-to ... --every 15`: every non-sample message and its
//! neighbouring samples are kept, other samples decimated to 4 Hz, seq
//! renumbered, module lists redacted, heartbeats dropped. Odometer distances
//! survive exactly. What happened in each is documented in docs/PLAN.md §3/§6.

use std::path::Path;

use pt_core::job::JobType;
use pt_core::rules::{Context, DiscardReason, Outcome, ReviewReason};
use pt_core::Config;
use pt_core::timeline::{GapKind, JobEvent, JobState, StartKind};
use pt_replay::{order_sessions, parse_lines, read_lines, replay, sample_config, Replayed};

fn run(name: &str) -> Vec<Replayed> {
    run_with(name, &sample_config(), &Context::default())
}

fn run_with(name: &str, cfg: &Config, ctx: &Context) -> Vec<Replayed> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real").join(name);
    let (mut frames, bad) = parse_lines(&read_lines(&path).expect("fixture"), cfg);
    assert_eq!(bad, 0);
    order_sessions(&mut frames);
    replay(&frames, cfg, ctx)
}

fn find<'a>(jobs: &'a [Replayed], cargo: &str) -> &'a Replayed {
    jobs.iter().find(|r| r.job.info.fingerprint.cargo.cargo_id == cargo).expect("job present")
}

fn count(events: &[JobEvent], f: impl Fn(&JobEvent) -> bool) -> usize {
    events.iter().filter(|e| f(e)).count()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// The owner's ATS session of 2026-10-16: warp tests in free roam (no job),
/// then a road trip (`car_job`) in an F-150 across New Orleans, 34 km, with
/// three loads. After the first the game sent the trip back with its deadline
/// a minute later; after the second it never sent it again, yet delivered it.
#[test]
fn ats_road_trip_across_loads() {
    let jobs = run("ats-road-trip-car-job-loads.jsonl.gz");
    assert_eq!(jobs.len(), 1, "one road trip, not split at the loads");
    let r = &jobs[0];
    assert_eq!(r.job.info.fingerprint.route.job_type, JobType::Car);
    assert_eq!(r.job.info.fingerprint.cargo.cargo_id, "xfer_seals");
    assert_eq!(r.job.state, JobState::Delivered);
    assert_eq!(r.job.delivered.as_ref().and_then(|d| d.distance_km), Some(34.0));
    // The untracked end of the trip is a gap, so a moderator sees it.
    assert!(r.result.review.iter().any(|x| matches!(x, ReviewReason::Gap { .. })), "{:?}", r.result.review);
    // Up to 163 mph in the pickup: over the road-trip penalty limit (155 mph)
    // for more than 3 s, under its invalid limit (200 mph). 3.41 mi x 0.85.
    assert_eq!(r.result.speed.over_invalid_s, 0.0);
    assert!(r.result.speed.penalty && !r.result.speed.invalid, "{:?}", r.result.speed);
    assert_eq!(r.result.outcome, Outcome::PendingReview);
    assert_eq!(r.result.points, 2.8);
    // No warp while on the trip.
    assert!(!r.result.review.iter().any(|x| matches!(x, ReviewReason::TimeWarp { .. })));
}

/// Chicago → Chicago, planned 70 km: the game reported 71 km (game scale).
#[test]
fn chicago_clean_delivery() {
    let jobs = run("ats-chicago-clean-delivery.jsonl.gz");
    assert_eq!(jobs.len(), 1);
    let r = &jobs[0];
    assert_eq!((r.job.state, r.job.start), (JobState::Delivered, StartKind::Tracked));
    let d = &r.result.distances;
    assert!(close(d.tracked_counted_game_km, 70.72, 0.01));
    assert!(close(d.tracked_uncounted_game_km, 0.75, 0.01));
    assert!(close(d.driven_counted_world_km, 5.929, 0.002));
    assert_eq!(d.delivered_game_km, Some(71.0));
    assert_eq!(r.result.outcome, Outcome::Valid, "{:?}", r.result.review);
    assert_eq!(r.result.points, 3.6);
}

/// Chicago → Sioux Falls on TruckersMP, planned 1042 km: a 159 km empty drive
/// to the pickup, a game restart mid-job and an autosave load before the
/// pickup. The game reported 1233 km for the whole trip.
#[test]
fn sioux_falls_empty_leg_restart_and_autosave() {
    let jobs = run("ats-sioux-falls-tmp-empty-leg.jsonl.gz");
    let r = find(&jobs, "dynamite");
    assert_eq!((r.job.state, r.job.start), (JobState::Delivered, StartKind::Tracked));
    assert_eq!(count(&r.job.events, |e| matches!(e, JobEvent::SessionContinue { .. })), 1);
    assert_eq!(count(&r.job.events, |e| matches!(e, JobEvent::LoadTruncate { .. })), 1);
    let d = &r.result.distances;
    assert!(close(d.tracked_whole_game_km, 1232.13, 0.02));
    assert!(close(d.tracked_uncounted_game_km, 158.84, 0.02));
    assert!(close(d.counted_game_km, 1074.16, 0.02));
    assert!(r.result.truckersmp && !r.result.convoy);
    assert_eq!(r.result.outcome, Outcome::Valid, "{:?}", r.result.review);
    assert_eq!(r.result.points, 36.1);
    // The beverages job was cancelled at the start of the session.
    assert_eq!(find(&jobs, "beverages").job.state, JobState::Cancelled);
}

/// Port Angeles → Tacoma on TruckersMP: six loads between saves 139 and 143
/// (including loading forward onto reverted material), a ferry, an autosave
/// load back across it, F7, and a cancel.
#[test]
fn six_loads_ferry_f7_cancel() {
    let jobs = run("ats-tmp-six-loads-ferry-f7-cancel.jsonl.gz");
    let r = find(&jobs, "tyres");
    let ev = &r.job.events;
    assert_eq!(count(ev, |e| matches!(e, JobEvent::LoadTruncate { .. })), 4);
    assert_eq!(count(ev, |e| matches!(e, JobEvent::LoadRestore { .. })), 2, "143 after 139 restores");
    assert_eq!(count(ev, |e| matches!(e, JobEvent::Teleport { ferry_or_train: true, .. })), 1);
    assert_eq!(count(ev, |e| matches!(e, JobEvent::Teleport { ferry_or_train: false, .. })), 1, "F7");
    assert!(r.job.gaps.is_empty());
    assert_eq!(r.job.state, JobState::Cancelled);
    assert!(r.result.review.is_empty(), "loads and teleports raise nothing: {:?}", r.result.review);
    assert!(close(r.result.distances.tracked_whole_game_km, 33.77, 0.02));
}

/// The tampered run: a save with the odometer ×10, a developer-mode teleport to
/// the destination and a save-edited 1000 km delivery of a 7 km job. Also two
/// refuels and a profile switch (no profile notices yet, so it falls back to
/// load handling).
#[test]
fn save_edits_are_held_for_review() {
    let jobs = run("ats-save-edits-profile-refuel.jsonl.gz");
    let r = find(&jobs, "beverages");
    assert_eq!(r.job.state, JobState::Delivered);
    assert_eq!(r.result.outcome, Outcome::PendingReview);
    assert!(r.result.review.iter().any(|x| matches!(x, ReviewReason::DistanceMismatch { delivered_km, .. } if *delivered_km == 1000.0)));
    assert_eq!(r.job.gaps.iter().filter(|g| g.kind == GapKind::Load).count(), 1, "the odometer edit");
    assert_eq!(r.result.refuels.len(), 2);
    assert!(!r.result.review.iter().any(|x| matches!(x, ReviewReason::FuelJump { .. } | ReviewReason::NegativeFuel { .. })));
    assert_eq!(count(&r.job.events, |e| matches!(e, JobEvent::Teleport { .. })), 2);
    // The other profile's quick job is a separate (late-start) job.
    assert_eq!(find(&jobs, "moving_cont").job.info.fingerprint.route.job_market, "quick_job");
}

/// ETS2 1.61.1.1s on TruckersMP (2026-10-13; account ID replaced). Four jobs in one
/// session: a Norway job continued from an earlier session (four ferries,
/// delivered, a manual save loaded seven seconds later and delivered again);
/// Dover → Calais through the Channel Tunnel with the empty drive to the
/// pickup; a 1 km quick job interrupted by a switch to another profile (whose
/// own quick job was accepted) and back.
#[test]
fn ets2_truckersmp_norway_tunnel_and_profiles() {
    let jobs = run("ets2-tmp-norway-tunnel-profiles.jsonl.gz");
    assert_eq!(jobs.len(), 4);
    for r in &jobs {
        assert!(r.result.truckersmp && !r.result.convoy, "TruckersMP SDK says connected");
    }

    // Loaded back into a delivered job and delivered again: counted once.
    let norway = jobs.iter().find(|r| r.job.info.fingerprint.route.source_city == "honningsvag").expect("norway");
    assert_eq!((norway.job.state, norway.job.start), (JobState::Stopped, StartKind::LateStart));
    assert_eq!(count(&norway.job.events, |e| matches!(e, JobEvent::Delivered { .. })), 1);
    assert_eq!(count(&norway.job.events, |e| matches!(e, JobEvent::DuplicateDelivery { .. })), 1);
    assert_eq!(norway.result.distances.delivered_game_km, Some(2512.0));
    assert_eq!(norway.result.outcome, Outcome::PendingReview);
    assert_eq!(norway.result.review, vec![ReviewReason::LateStart]);

    // The tunnel is a teleport and adds nothing; distance.km leaves it out too
    // (239 reported, 238.87 tracked). UK roads are scale 15.
    let tunnel = jobs.iter().find(|r| r.job.info.fingerprint.route.source_city == "dover").expect("dover");
    assert_eq!((tunnel.job.state, tunnel.job.start), (JobState::Delivered, StartKind::Tracked));
    assert_eq!(count(&tunnel.job.events, |e| matches!(e, JobEvent::Teleport { ferry_or_train: true, .. })), 1);
    let d = &tunnel.result.distances;
    assert_eq!(d.delivered_game_km, Some(239.0));
    assert!(close(d.tracked_whole_game_km, 238.87, 0.01));
    assert!(close(d.tracked_uncounted_game_km, 113.30, 0.01), "empty drive to the pickup");
    assert_eq!(tunnel.result.outcome, Outcome::Valid, "{:?}", tunnel.result.review);
    assert_eq!(tunnel.result.points, 7.0);

    // 1 km quick job: tracked 1.8 km against a whole-km report of 1 km is within
    // the reporting resolution. The return to its profile reloads its autosave.
    let quick = find(&jobs, "med_equip");
    assert_eq!(quick.job.state, JobState::Delivered);
    assert_eq!(count(&quick.job.events, |e| matches!(e, JobEvent::LoadTruncate { .. })), 1);
    // It paid 388 for 1 km (624 a mile): over the income limit, which applies to
    // every job however short (owner, 2026-10-16).
    assert_eq!(quick.result.outcome, Outcome::PendingReview);
    assert!(matches!(quick.result.review[..], [ReviewReason::HighIncome { income: 388, .. }]), "{:?}", quick.result.review);

    // The other profile's quick job, accepted right after its autosave loaded
    // (no job in the save), was seen from its start; it was never delivered.
    let apples = find(&jobs, "apples");
    assert_eq!(apples.job.start, StartKind::Tracked);
    assert_eq!(apples.result.outcome, Outcome::Incomplete);
}

/// ETS2 1.61.1.1s, a Convoy hosted alone (2026-10-13): a quick job accepted
/// right after the start autosave loaded, then the Convoy session started and
/// the job was driven about 80 s. TruckersMP was not loaded.
#[test]
fn ets2_convoy_with_a_quick_job() {
    let jobs = run("ets2-convoy-quick-job.jsonl.gz");
    assert_eq!(jobs.len(), 1);
    let r = &jobs[0];
    assert_eq!(r.job.info.fingerprint.cargo.cargo_id, "hvac");
    assert_eq!((r.job.state, r.job.start), (JobState::Tracking, StartKind::Tracked));
    assert!(r.result.convoy && !r.result.truckersmp, "multiplayer.time.offset 3..5, no TruckersMP");
    assert!(r.job.events.iter().all(|e| !matches!(e, JobEvent::LoadTruncate { .. } | JobEvent::LoadGap { .. })), "hosting is not a load");
    assert_eq!(r.result.outcome, Outcome::Incomplete);
}

/// The owner's first HaulMP job (ETS2 1.61.1.9s, HaulMP v814, Traffic server,
/// 2026-10-22): Dover → Groningen across three game sessions, with the Dover →
/// Calais ferry. HaulMP keeps `multiplayer.time.offset` at 0 and runs the game
/// clock at `local.scale`, so only its module names (kept by redaction as
/// `mod_matches`) tell it from singleplayer.
#[test]
fn ets2_haulmp_job_across_sessions() {
    let name = "ets2-haulmp-ferry-three-sessions.jsonl.gz";
    let jobs = run(name);
    assert_eq!(jobs.len(), 1);
    let r = &jobs[0];
    assert_eq!(r.job.info.fingerprint.cargo.cargo_id, "jcb_pw125qe");
    assert_eq!((r.job.state, r.job.start), (JobState::Delivered, StartKind::Tracked));
    let ev = &r.job.events;
    assert_eq!(count(ev, |e| matches!(e, JobEvent::Teleport { ferry_or_train: true, .. })), 1, "the ferry");
    assert_eq!(count(ev, |e| matches!(e, JobEvent::SessionContinue { .. })), 2, "two game restarts");
    assert!(r.job.gaps.is_empty());
    assert_eq!(r.result.multiplayer_mod.as_deref(), Some("haulmp"));
    assert!(!r.result.truckersmp && !r.result.convoy);
    assert_eq!(r.result.distances.delivered_game_km, Some(603.0));
    assert!(close(r.result.distances.driven_counted_world_km, 36.248, 0.002));
    assert_eq!(r.result.outcome, Outcome::Valid, "{:?}", r.result.review);
    assert_eq!(r.result.points, 22.5);

    // The Singleplayer Division discards it, as configured for HaulMP.
    let sp = Context { singleplayer_division: true, ..Context::default() };
    let r = &run_with(name, &sample_config(), &sp)[0];
    assert_eq!(r.result.outcome, Outcome::Discarded);
    assert_eq!(r.result.discard, Some(DiscardReason::SingleplayerDivisionMultiplayerMod));
    assert_eq!(r.result.points, 0.0);

    // With the toggle off it counts for them too.
    let mut keep = sample_config();
    keep.multiplayer.mods.iter_mut().for_each(|m| m.singleplayer_division_discard = false);
    assert_eq!(run_with(name, &keep, &sp)[0].result.outcome, Outcome::Valid);

    // A config without HaulMP sees singleplayer, as clients did before.
    let mut none = sample_config();
    none.multiplayer.mods.clear();
    let r = &run_with(name, &none, &sp)[0];
    assert_eq!(r.result.multiplayer_mod, None);
    assert!(!r.result.truckersmp && !r.result.convoy);
    assert_eq!(r.result.outcome, Outcome::Valid);
}

/// The server snapshots the timeline between batches (PLAN §7a): stopping
/// anywhere, serialising and continuing must give exactly the same jobs.
#[test]
fn a_snapshot_mid_capture_replays_identically() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real/ets2-tmp-norway-tunnel-profiles.jsonl.gz");
    let cfg = sample_config();
    let (mut frames, _) = parse_lines(&read_lines(&path).expect("fixture"), &cfg);
    order_sessions(&mut frames);
    let straight = replay(&frames, &cfg, &Context::default());
    for cut in [frames.len() / 3, frames.len() / 2, frames.len() * 9 / 10] {
        let mut a = pt_core::timeline::Profiles::new(cfg.clone());
        for f in &frames[..cut] {
            a.push(f);
        }
        let json = serde_json::to_string(&a).expect("serialise");
        let mut b: pt_core::timeline::Profiles = serde_json::from_str(&json).expect("deserialise");
        for f in &frames[cut..] {
            b.push(f);
        }
        let jobs = b.into_jobs();
        assert_eq!(jobs.len(), straight.len());
        for (j, s) in jobs.iter().zip(&straight) {
            assert_eq!(j, &s.job, "cut at {cut}");
            assert_eq!(pt_core::rules::evaluate(j, &cfg, &Context::default()), s.result);
        }
    }
}
