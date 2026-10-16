//! Replay fixtures for every case in PLAN §10 plus the decisions of PLAN §11.
//! Each test drives the simulator (tests/sim) and checks the job the shared
//! timeline and rules produce.

mod sim;

use pt_core::rules::{Bonus, BonusKind, Context, DiscardReason, Outcome, ReviewReason};
use pt_core::timeline::{GapKind, JobEvent, JobState, StartKind};
use pt_core::job::JobType;
use pt_core::Game;
use sim::{rescore, JobSpec, Sim};

fn has<F: Fn(&JobEvent) -> bool>(events: &[JobEvent], f: F) -> bool {
    events.iter().any(f)
}

fn reasons(r: &pt_core::rules::JobResult) -> Vec<String> {
    r.review.iter().map(|x| format!("{x:?}")).collect()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// Accept with an empty drive to the pickup, load, drive, deliver with the
/// game's figure = the whole trip (as measured on the Sioux Falls job).
#[test]
fn clean_delivery_scores_loaded_distance_at_1_to_1() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("dynamite").empty_leg());
    s.drive_km(10.0);
    s.load_cargo();
    s.drive_km(200.0);
    s.deliver(210.0);
    let (job, r) = s.job("dynamite");
    assert_eq!(job.state, JobState::Delivered);
    assert_eq!(job.start, StartKind::Tracked);
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(close(r.distances.tracked_uncounted_game_km, 10.0, 0.05));
    assert!(close(r.distances.counted_game_km, 200.0, 0.05));
    // 200 game km at scale 20 = 10 km 1:1 = 6.21371 mi -> 6.2 points.
    assert!(close(r.distances.world_km, 10.0, 0.01));
    assert_eq!(r.points, 6.2);
}

// ---- spec boundaries ---------------------------------------------------------

fn speed_run(game: &'static str, mph: f64, secs: f64) -> pt_core::rules::JobResult {
    let mut s = Sim::new(game);
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.drive_secs(secs, mph);
    s.drive_km(5.0);
    let whole = s.odo;
    let _ = whole;
    s.deliver(10.0);
    s.job("tyres").1
}

#[test]
fn ats_speed_limits_exactly_on_the_limit_are_not_over() {
    let r = speed_run("ats", 105.0, 10.0);
    assert!(!r.speed.penalty, "exactly 105.0 mph is not over 105");
    let r = speed_run("ats", 105.01, 4.0);
    assert!(r.speed.penalty && !r.speed.invalid);
    let r = speed_run("ats", 125.0, 10.0);
    assert!(!r.speed.invalid, "exactly 125.0 mph is not over 125");
    let r = speed_run("ats", 125.01, 4.0);
    assert!(r.speed.invalid);
    assert_eq!(r.outcome, Outcome::Invalid);
    assert_eq!(r.points, 0.0);
}

#[test]
fn ets2_speed_limits_exactly_on_the_limit_are_not_over() {
    assert!(!speed_run("eut2", 100.0, 10.0).speed.penalty);
    assert!(speed_run("eut2", 100.01, 4.0).speed.penalty);
    assert!(!speed_run("eut2", 150.0, 10.0).speed.invalid);
    assert!(speed_run("eut2", 150.01, 4.0).speed.invalid);
}

#[test]
fn a_one_second_spike_is_not_sustained_overspeed() {
    let r = speed_run("ats", 140.0, 1.0);
    assert!(!r.speed.invalid && !r.speed.penalty);
    assert!(r.speed.peak_mph > 139.0);
    let r = speed_run("ats", 140.0, 3.5);
    assert!(r.speed.invalid);
}

#[test]
fn pressing_esc_repeatedly_does_not_reset_overspeed() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    for _ in 0..3 {
        s.drive_secs(1.5, 110.0);
        s.pause();
        s.start();
    }
    s.drive_km(5.0);
    s.deliver(10.0);
    let (_, r) = s.job("tyres");
    assert!(r.speed.penalty, "4.5 s over 105 split by pauses is still sustained");
}

#[test]
fn overspeed_across_a_ferry_is_split() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.drive_secs(2.0, 110.0);
    s.ferry(1300.0);
    s.drive_secs(2.0, 110.0);
    s.drive_km(5.0);
    s.deliver(10.0);
    let (job, r) = s.job("tyres");
    assert!(!r.speed.penalty);
    assert!(has(&job.events, |e| matches!(e, JobEvent::Teleport { ferry_or_train: true, .. })));
}

#[test]
fn review_threshold_150_vs_150_1_world_km() {
    for (delivered, over) in [(150.0, false), (150.1, true)] {
        let mut s = Sim::new("ats");
        s.scale = 1.0; // a 1:1 map: game km = world km
        s.accept(JobSpec::new("tyres"));
        s.drive_km(150.0);
        s.deliver(delivered);
        let (_, r) = s.job("tyres");
        let long = r.review.iter().any(|x| matches!(x, ReviewReason::LongJob { .. }));
        assert_eq!(long, over, "delivered {delivered}: {:?}", r.review);
    }
}

#[test]
fn fuel_jump_within_tolerance_is_silent_beyond_it_is_reviewed() {
    for (jump, flagged) in [(0.3, false), (1.0, true)] {
        let mut s = Sim::new("ats");
        s.accept(JobSpec::new("tyres"));
        s.drive_km(5.0);
        s.fuel += jump;
        s.drive_km(5.0);
        s.deliver(10.0);
        let (_, r) = s.job("tyres");
        let fuel = r.review.iter().any(|x| matches!(x, ReviewReason::FuelJump { .. } | ReviewReason::NegativeFuel { .. }));
        assert_eq!(fuel, flagged, "jump {jump}: {:?}", r.review);
        if flagged {
            assert_eq!(r.outcome, Outcome::PendingReview, "negative fuel holds the points for review");
        }
    }
}

#[test]
fn a_legitimate_refuel_is_detected_and_not_flagged() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.idle(2.0);
    s.refuel(200.0, 54.0); // the fast phase measured in ATS 1.61
    s.drive_km(5.0);
    s.deliver(10.0);
    let (_, r) = s.job("tyres");
    assert_eq!(r.refuels.len(), 1);
    assert!(close(r.refuels[0].litres, 200.0, 3.0));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn negative_distance_is_discarded() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.odometer_step(-1.0);
    s.drive_km(5.0);
    s.deliver(9.0);
    let (_, r) = s.job("tyres");
    assert_eq!(r.outcome, Outcome::Discarded);
    assert_eq!(r.discard, Some(DiscardReason::NegativeDistance));
}

#[test]
fn a_cancelled_job_is_incomplete() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.cancel();
    s.drive_km(1.0);
    let (job, r) = s.job("tyres");
    assert_eq!(job.state, JobState::Cancelled);
    assert_eq!(r.outcome, Outcome::Incomplete);
}

#[test]
fn a_crash_mid_job_continues_or_loads_the_autosave() {
    // Restarted where it was: the job just continues, distance bridged.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    s.crash(None);
    s.drive_km(20.0);
    s.deliver(40.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::SessionContinue { .. })));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(close(r.distances.tracked_whole_game_km, 40.0, 0.05));

    // Restarted from an older autosave: a load back to it.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let autosave = s.save();
    s.drive_km(20.0);
    s.crash(Some(&autosave));
    s.drive_km(20.0);
    s.deliver(30.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::LoadTruncate { .. })));
    assert!(close(r.distances.tracked_whole_game_km, 30.0, 0.05));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn the_greatest_bonus_applies_and_stacks_with_the_penalty() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("dynamite"));
    s.drive_km(100.0);
    s.drive_secs(4.0, 110.0);
    s.drive_km(100.0);
    let whole: f64 = 200.0 + 4.0 * 110.0 / 2.23694 * 20.0 / 1000.0;
    s.deliver(whole.round());
    let (job, _) = s.job("dynamite");
    let bonus = |kind, amount, m: &str| Bonus {
        id: String::new(),
        exact: false,
        game: Game::Ats,
        cargo_match: m.into(),
        kind,
        amount,
        active_from_ms: None,
        active_to_ms: None,
    };
    let ctx = Context {
        bonuses: vec![
            bonus(BonusKind::Multiplier, 2.0, "dynamite"),
            bonus(BonusKind::Flat, 100.0, "dyna*"),
            bonus(BonusKind::Flat, 500.0, "tyres"),
        ],
        ..Default::default()
    };
    let r = rescore(&job, &ctx);
    assert!(r.speed.penalty);
    let accepted = r.distances.world_mi * 0.85;
    assert!(close(r.accepted_world_mi, accepted, 1e-9));
    assert_eq!(r.bonus.as_ref().map(|b| b.kind), Some(BonusKind::Flat), "flat 100 beats x2 on ~9 mi");
    assert_eq!(r.points, ((accepted + 100.0) * 10.0).floor() / 10.0);
}

#[test]
fn an_ets2_job_uses_scale_19_and_km() {
    let mut s = Sim::new("eut2");
    s.accept(JobSpec::new("apples").to("paris"));
    s.drive_km(190.0);
    s.deliver(190.0);
    let (job, r) = s.job("apples");
    assert_eq!(job.game, Game::Ets2);
    assert!(close(r.distances.world_km, 10.0, 0.01), "190 game km at 1:19 = 10 km");
    assert_eq!(r.points, 6.2);
}

// ---- save-load scenarios -------------------------------------------------------

fn future(s: &Sim, game_km: f64) -> sim::Snap {
    let mut f = s.save();
    f.odo += game_km;
    f.pos[0] += game_km * 1000.0 / s.scale;
    f.fuel -= 10.0;
    f.gt += 90.0;
    f
}

#[test]
fn load_forward_on_the_same_job_then_deliver() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(50.0);
    let ahead = future(&s, 100.0);
    s.load(&ahead);
    s.drive_km(50.0);
    s.deliver(200.0);
    let (job, r) = s.job("tyres");
    assert_eq!(job.gaps.iter().filter(|g| g.kind == GapKind::Load).count(), 1);
    assert!(reasons(&r).iter().any(|x| x.starts_with("Gap")), "{:?}", r.review);
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::DistanceMismatch { .. })), "gap counted in the whole trip");
    assert_eq!(r.outcome, Outcome::PendingReview);
}

#[test]
fn load_forward_across_a_refuel_raises_no_fuel_flag() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(50.0);
    let mut ahead = future(&s, 100.0);
    ahead.fuel += 300.0;
    s.load(&ahead);
    s.drive_km(50.0);
    s.deliver(200.0);
    let (_, r) = s.job("tyres");
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::FuelJump { .. } | ReviewReason::NegativeFuel { .. })), "{:?}", r.review);
}

#[test]
fn load_a_no_job_save_then_forward_back_into_the_job() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(30.0);
    let mut nojob = s.save();
    nojob.job = None;
    nojob.pos[0] += 20_000.0;
    let ahead = future(&s, 40.0);
    s.load(&nojob);
    s.drive_km(5.0);
    s.load(&ahead);
    s.drive_km(30.0);
    s.deliver(100.0);
    let (job, _) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::Suspended { .. })));
    assert!(has(&job.events, |e| matches!(e, JobEvent::Resumed { .. })));
    assert_eq!(job.state, JobState::Delivered);
}

#[test]
fn client_closed_game_loads_a_future_save_client_restarted() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(30.0);
    let ahead = future(&s, 60.0);
    s.crash(Some(&ahead));
    s.drive_km(30.0);
    s.deliver(120.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::LoadGap { .. })));
    assert!(reasons(&r).iter().any(|x| x.starts_with("Gap")));
}

#[test]
fn load_forward_then_back_past_the_gap_reverts_the_gap() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let before_gap = s.save();
    s.drive_km(10.0);
    let ahead = future(&s, 100.0);
    s.load(&ahead);
    s.drive_km(10.0);
    s.load(&before_gap);
    s.drive_km(20.0);
    s.deliver(30.0);
    let (job, r) = s.job("tyres");
    assert_eq!(job.gaps.len(), 1, "the gap stays in the trace");
    assert_eq!(job.active_gaps().count(), 0, "but it was reverted");
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(close(r.distances.tracked_whole_game_km, 30.0, 0.05));
}

#[test]
fn load_forward_onto_a_different_jobs_save() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    let mut other = future(&s, 50.0);
    other.job = Some(JobSpec::new("bricks").to("denver"));
    s.load(&other);
    s.drive_km(10.0);
    let jobs = s.replay();
    let tyres = jobs.iter().find(|r| r.job.info.fingerprint.cargo.cargo_id == "tyres").unwrap();
    let bricks = jobs.iter().find(|r| r.job.info.fingerprint.cargo.cargo_id == "bricks").unwrap();
    assert_eq!(tyres.job.state, JobState::Suspended);
    assert!(tyres.result.review.is_empty(), "suspended, not penalised");
    assert_eq!(bricks.job.start, StartKind::LateStart);
}

#[test]
fn load_into_an_already_delivered_job_stops_tracking() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let mid = s.save();
    s.drive_km(10.0);
    s.deliver(20.0);
    s.load(&mid);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert_eq!(job.state, JobState::Stopped);
    assert!(has(&job.events, |e| matches!(e, JobEvent::StoppedAfterDelivery { .. })));
    assert!(has(&job.events, |e| matches!(e, JobEvent::DuplicateDelivery { .. })));
    assert_eq!(r.outcome, Outcome::Valid, "the first delivery stands: {:?}", r.review);
    assert!(close(r.distances.tracked_whole_game_km, 20.0, 0.05), "nothing after the stop is recorded");
}

#[test]
fn a_late_start_takes_the_games_distance_and_goes_to_review() {
    let mut s = Sim::with_job("ats", Some(JobSpec::new("tyres")));
    s.drive_km(40.0);
    s.deliver(100.0); // the game knows the 60 km driven before tracking began
    let (job, r) = s.job("tyres");
    assert_eq!(job.start, StartKind::LateStart);
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::LateStart)));
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::DistanceMismatch { .. })));
    assert!(close(r.distances.counted_game_km, 100.0, 1e-9));
    assert!(close(r.distances.world_km, 5.0, 0.01));
}

#[test]
fn an_invalid_status_is_cleared_by_a_load() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let before = s.save();
    s.drive_secs(5.0, 200.0); // falling under the map
    s.load(&before);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(!r.speed.invalid);
    assert!(job.reverted.iter().any(|b| b.points.iter().any(|p| p.mph() > 190.0)), "kept as evidence");
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn a_delivered_invalid_job_is_reopened_by_a_load_then_delivered_clean() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let before = s.save();
    s.drive_secs(5.0, 200.0);
    s.drive_km(5.0);
    s.deliver(15.0);
    assert_eq!(s.job("tyres").1.outcome, Outcome::Invalid);
    s.load(&before);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::InvalidDeliveryReopened { .. })));
    assert_eq!(job.reverted_deliveries.len(), 1);
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn singleplayer_division_discards_truckersmp_but_scores_convoy() {
    let mut s = Sim::new("ats");
    s.truckersmp();
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(r.truckersmp && !r.convoy);
    assert_eq!(r.outcome, Outcome::Valid, "tracked like any job");
    let sp = rescore(&job, &Context { singleplayer_division: true, ..Default::default() });
    assert_eq!(sp.discard, Some(DiscardReason::SingleplayerDivisionTruckersmp));

    let mut s = Sim::new("ats");
    s.mp_offset = 24_136_737;
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(r.convoy && !r.truckersmp);
    let sp = rescore(&job, &Context { singleplayer_division: true, ..Default::default() });
    assert_eq!(sp.outcome, Outcome::Valid);
}

#[test]
fn a_profile_switch_mid_job_keeps_separate_timelines() {
    let mut s = Sim::new("ats");
    s.profile("P1");
    s.idle(0.5); // the profile's world is sampled before a job is taken
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let p1 = s.save();
    let mut p2 = s.save();
    p2.job = Some(JobSpec::new("bricks").to("denver"));
    p2.pos[0] += 50_000.0;
    p2.odo = 500.0;
    s.profile("P2");
    s.load(&p2);
    s.drive_km(10.0);
    s.profile("P1");
    s.load(&p1);
    s.drive_km(10.0);
    s.deliver(20.0);
    let jobs = s.replay();
    let tyres = jobs.iter().find(|r| r.job.info.fingerprint.cargo.cargo_id == "tyres").unwrap();
    let bricks = jobs.iter().find(|r| r.job.info.fingerprint.cargo.cargo_id == "bricks").unwrap();
    assert_eq!(tyres.job.profile, "P1");
    assert_eq!(bricks.job.profile, "P2");
    assert!(!has(&tyres.job.events, |e| matches!(e, JobEvent::Suspended { .. })), "{:?}", tyres.job.events);
    assert_eq!(tyres.result.outcome, Outcome::Valid, "{:?}", tyres.result.review);
    assert!(close(tyres.result.distances.tracked_whole_game_km, 20.0, 0.05));
}

#[test]
fn a_distance_the_trace_cannot_support_goes_to_review() {
    // The captured save edit: a 7 km job delivered as 1000 km.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("beverages"));
    s.drive_km(7.0);
    s.deliver(1000.0);
    let (_, r) = s.job("beverages");
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::DistanceMismatch { .. })), "{:?}", r.review);
    assert_eq!(r.outcome, Outcome::PendingReview);
}

// ---- red-team cases --------------------------------------------------------------

#[test]
fn revert_then_restore_keeps_the_overspeed() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let a = s.save();
    s.drive_secs(5.0, 110.0);
    s.drive_km(2.0);
    let b = s.save();
    s.load(&a);
    s.idle(1.0); // the game always samples after a load
    s.load(&b); // back onto the reverted branch
    s.drive_km(10.0);
    let whole = s.odo;
    let _ = whole;
    s.deliver(22.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::LoadRestore { .. })));
    assert!(r.speed.penalty, "the restored overspeed counts");
}

#[test]
fn accepting_a_job_or_a_quick_job_is_not_a_load() {
    let mut s = Sim::new("ats");
    s.drive_km(2.0);
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.deliver(5.0);
    s.switch_truck("vehicle.freightliner.cascadia2024", 0.085);
    s.accept(JobSpec::new("moving_cont").market("quick_job").to("las_vegas"));
    s.drive_km(5.0);
    s.deliver(5.0);
    for r in s.replay() {
        assert_eq!(r.job.start, StartKind::Tracked, "{:?}", r.job.events);
        assert!(!has(&r.job.events, |e| matches!(e, JobEvent::LoadTruncate { .. } | JobEvent::LoadGap { .. } | JobEvent::LoadRestore { .. })));
        assert_eq!(r.result.outcome, Outcome::Valid, "{:?}", r.result.review);
    }
}

#[test]
fn delivery_and_cancel_ordering_attribute_to_the_last_job() {
    // deliver(): event before the config empties; cancel(): config empties first.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(5.0);
    s.cancel();
    s.accept(JobSpec::new("bricks").to("denver"));
    s.drive_km(5.0);
    s.deliver(5.0);
    assert_eq!(s.job("tyres").0.state, JobState::Cancelled);
    assert_eq!(s.job("bricks").0.state, JobState::Delivered);
}

#[test]
fn a_delivery_while_the_client_was_closed_still_scores() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    let cut = s.lines.len();
    s.drive_km(10.0);
    let resume = s.lines.len();
    s.drive_km(10.0);
    s.deliver(40.0);
    // The plugin journals durable messages but samples older than its replay
    // buffer are lost: drop 10 km of samples.
    s.lines.drain(cut..resume);
    let (job, r) = s.job("tyres");
    assert_eq!(job.state, JobState::Delivered);
    assert!(job.gaps.iter().any(|g| g.kind == GapKind::Bridge));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(close(r.distances.tracked_whole_game_km, 40.0, 0.05));
}

#[test]
fn a_truck_swap_does_nothing() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    s.switch_truck("vehicle.kenworth.w900", 50_000.0);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::TruckChange { .. })));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(close(r.distances.tracked_whole_game_km, 20.0, 0.05), "the new odometer is not distance");
}

#[test]
fn a_save_with_another_truck_matches_by_position() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let mut edited = s.save(); // save-edited truck (C20), same place
    edited.truck = "vehicle.peterbilt.579".into();
    edited.odo = 77_777.0;
    s.drive_km(5.0);
    s.load(&edited);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(has(&job.events, |e| matches!(e, JobEvent::LoadTruncate { .. })), "{:?}", job.events);
    assert!(job.gaps.is_empty());
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn pausing_in_a_menu_after_a_load_is_one_load() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let a = s.save();
    s.drive_km(5.0);
    s.load(&a);
    for _ in 0..5 {
        s.pause();
        s.start();
    }
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    let loads = job.events.iter().filter(|e| matches!(e, JobEvent::LoadTruncate { .. })).count();
    assert_eq!(loads, 1);
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

// ---- decisions of 2026-09-28 --------------------------------------------------------

#[test]
fn teleports_add_no_distance_and_are_never_reviewed() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    s.teleport(1800.0); // developer-mode free-camera teleport
    s.drive_km(10.0);
    s.teleport(6400.0); // F7
    s.drive_km(10.0);
    s.deliver(30.0);
    let (job, r) = s.job("tyres");
    assert_eq!(job.events.iter().filter(|e| matches!(e, JobEvent::Teleport { .. })).count(), 2);
    assert!(close(r.distances.tracked_whole_game_km, 30.0, 0.05));
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn a_road_trip_is_tracked_like_freight() {
    // ATS road trips (2026-10-16): `car_job` instead of `job`.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("xfer_seals").road_trip());
    s.drive_km(30.0);
    s.deliver(30.0);
    let (job, r) = s.job("xfer_seals");
    assert_eq!(job.info.fingerprint.route.job_type, JobType::Car);
    assert_eq!(job.info.fingerprint.route.job_market, "dispatch_job");
    assert_eq!(job.state, JobState::Delivered);
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
    assert!(r.points > 0.0);
}

#[test]
fn road_trips_have_their_own_speed_limits() {
    // ATS road trips (owner, 2026-10-16): penalty over 155 mph, invalid over 200.
    let trip = |mph: f64, road_trip: bool| {
        let mut s = Sim::new("ats");
        let spec = JobSpec::new("xfer_seals");
        s.accept(if road_trip { spec.road_trip() } else { spec });
        s.drive_secs(60.0, 60.0);
        s.drive_secs(10.0, mph);
        s.drive_secs(60.0, 60.0);
        s.deliver(30.0);
        s.job("xfer_seals").1
    };
    let r = trip(150.0, true);
    assert!(!r.speed.penalty && !r.speed.invalid, "{:?}", r.speed);
    let r = trip(160.0, true);
    assert!(r.speed.penalty && !r.speed.invalid, "{:?}", r.speed);
    let r = trip(205.0, true);
    assert!(r.speed.invalid, "{:?}", r.speed);
    assert_eq!(r.outcome, Outcome::Invalid);
    // A truck at 150 mph is still invalid (ATS: over 125).
    let r = trip(150.0, false);
    assert!(r.speed.invalid, "{:?}", r.speed);
}

#[test]
fn a_cancelled_road_trip_is_cancelled() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("xfer_seals").road_trip());
    s.drive_km(5.0);
    s.cancel();
    assert_eq!(s.job("xfer_seals").0.state, JobState::Cancelled);
}

#[test]
fn a_freight_delivery_never_ends_a_road_trip() {
    // The road trip's own event ends it; a stray freight event does not.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("xfer_seals").road_trip());
    s.drive_km(5.0);
    let (job, _) = s.job("xfer_seals");
    assert_eq!(job.state, JobState::Tracking);
    s.drive_km(5.0);
    s.deliver(10.0);
    assert_eq!(s.job("xfer_seals").0.state, JobState::Delivered);
}

#[test]
fn the_warp_command_is_held_for_review() {
    // The owner's ATS test (2026-10-16): under `warp 2` every game clock runs
    // twice as fast as the PC's; the truck's speed looks normal.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(120.0, 60.0);
    s.warp = 2.0;
    s.drive_secs(120.0, 60.0);
    s.warp = 1.0;
    s.drive_secs(60.0, 60.0);
    s.deliver(21.0);
    let (_, r) = s.job("tyres");
    assert_eq!(r.outcome, Outcome::PendingReview);
    let warp = r.review.iter().find_map(|x| match x {
        ReviewReason::TimeWarp { max_factor, gained_s } => Some((*max_factor, *gained_s)),
        _ => None,
    });
    let (factor, gained) = warp.expect("time_warp raised");
    assert!(factor > 1.5, "{factor}");
    assert!(gained > 40.0, "{gained}");
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { .. })), "the pace looks normal: {:?}", r.review);
}

#[test]
fn a_pause_does_not_hide_a_warp() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    for _ in 0..6 {
        s.warp = 2.0;
        s.drive_secs(25.0, 60.0);
        s.warp = 1.0;
        s.pause();
        s.wall_ms += 25_000;
        s.start();
    }
    s.deliver(21.0);
    let (_, r) = s.job("tyres");
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::TimeWarp { .. })), "{:?}", r.review);
}

#[test]
fn normal_driving_is_not_warp() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(600.0, 60.0);
    s.deliver(21.0);
    let (_, r) = s.job("tyres");
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::TimeWarp { .. })), "{:?}", r.review);
}

#[test]
fn a_portal_that_fills_the_tank_raises_no_fuel_flag() {
    // World of Trucks event portals (Brackenreach, 2026-10-15): the truck is
    // moved 258 km and the tank is full on arrival.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    s.fuel = 277.0;
    s.drive_km(1.0);
    s.fuel = 800.0;
    s.teleport(13_000.0);
    s.drive_km(10.0);
    s.deliver(21.0);
    let (_, r) = s.job("tyres");
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn bobtailing_does_not_count() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    s.connected = false;
    s.drive_km(5.0);
    s.connected = true;
    s.drive_km(10.0);
    s.deliver(25.0);
    let (_, r) = s.job("tyres");
    assert!(close(r.distances.tracked_uncounted_game_km, 5.0, 0.05));
    assert!(close(r.distances.counted_game_km, 20.0, 0.05));
}

#[test]
fn world_of_trucks_counts_the_empty_and_bobtail_driving() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres").market("external_contracts").empty_leg());
    s.drive_km(10.0);
    s.load_cargo();
    s.connected = false;
    s.drive_km(5.0);
    s.connected = true;
    s.drive_km(15.0);
    s.deliver(30.0);
    let (_, r) = s.job("tyres");
    assert!(close(r.distances.counted_game_km, 30.0, 1e-9));
}

#[test]
fn a_cargo_switch_goes_to_review_and_keeps_the_original_bonus_basis() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let mut edited = s.save();
    if let Some(j) = edited.job.as_mut() {
        j.cargo = "gold".into();
    }
    s.load(&edited);
    s.drive_km(10.0);
    s.deliver(20.0);
    let (job, r) = s.job("gold");
    assert!(job.cargo_switched);
    assert_eq!(job.first_info.fingerprint.cargo.cargo_id, "tyres");
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::CargoSwitch)));
    let ctx = Context {
        bonuses: vec![Bonus {
            id: String::new(),
            exact: false,
            game: Game::Ats,
            cargo_match: "gold".into(),
            kind: BonusKind::Multiplier,
            amount: 10.0,
            active_from_ms: None,
            active_to_ms: None,
        }],
        ..Default::default()
    };
    assert!(rescore(&job, &ctx).bonus.is_none(), "no bonus for the switched-in cargo");
}

#[test]
fn the_truckersmp_sdk_tells_server_play_from_launcher_singleplayer() {
    // TruckersMP loaded (its DLL is in the process) but not on a server:
    // singleplayer through the TruckersMP launcher is not TruckersMP.
    let mut s = Sim::new("ats");
    s.truckersmp();
    s.truckersmp_sdk(false);
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(!r.truckersmp);
    let sp = rescore(&job, &Context { singleplayer_division: true, ..Default::default() });
    assert_eq!(sp.outcome, Outcome::Valid);

    // Connected to a server: TruckersMP, discarded for the Singleplayer Division.
    let mut s = Sim::new("ats");
    s.truckersmp();
    s.truckersmp_sdk(true);
    s.accept(JobSpec::new("tyres"));
    s.drive_km(20.0);
    s.deliver(20.0);
    let (job, r) = s.job("tyres");
    assert!(r.truckersmp);
    let sp = rescore(&job, &Context { singleplayer_division: true, ..Default::default() });
    assert_eq!(sp.discard, Some(DiscardReason::SingleplayerDivisionTruckersmp));
}

#[test]
fn a_quick_job_taken_right_after_loading_a_jobless_save_is_tracked() {
    // ETS2 Convoy capture (2026-10-13): the loaded save had no job (job config
    // re-sent empty with a full truck), and a quick job was accepted 19 s later,
    // before the first sample.
    let mut s = Sim::new("ats");
    s.drive_km(5.0);
    let jobless = future(&s, 30.0);
    s.load(&jobless);
    s.accept(JobSpec::new("hvac").market("quick_job"));
    s.drive_km(20.0);
    s.deliver(20.0);
    let (job, r) = s.job("hvac");
    assert_eq!(job.start, StartKind::Tracked);
    assert_eq!(r.outcome, Outcome::Valid, "{:?}", r.review);
}

#[test]
fn a_save_with_a_job_loaded_after_a_jobless_one_is_still_a_late_start() {
    // Two loads in one pause: the second brings a job already in progress.
    let mut s = Sim::new("ats");
    s.drive_km(5.0);
    let jobless = future(&s, 30.0);
    let mut with_job = future(&s, 60.0);
    with_job.job = Some(JobSpec::new("bricks"));
    s.load(&jobless);
    s.load(&with_job);
    s.drive_km(10.0);
    let (job, _) = s.job("bricks");
    assert_eq!(job.start, StartKind::LateStart);
}

#[test]
fn a_whole_job_averaging_over_5_km_per_minute_goes_to_review() {
    // 200 mph = 5.36 1:1 km per real minute, and the same per in-game minute
    // in game km: both pace checks fire (2026-10-14).
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(180.0, 200.0);
    s.deliver(64.0);
    let (_, r) = s.job("tyres");
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { world_km_per_min, .. } if *world_km_per_min > 5.0)), "{:?}", r.review);
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::FastAverageGame { world_km_per_min, .. } if *world_km_per_min > 5.0)), "{:?}", r.review);
}

#[test]
fn normal_driving_and_pauses_do_not_trip_the_pace_checks() {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(300.0, 60.0);
    s.pause();
    s.idle(0.0);
    s.drive_secs(300.0, 60.0);
    s.deliver(40.0);
    let (_, r) = s.job("tyres");
    assert!(
        !r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { .. } | ReviewReason::FastAverageGame { .. })),
        "{:?}",
        r.review
    );
}

#[test]
fn a_short_burst_is_not_averaged() {
    // Under a minute of driving: too short to average.
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(30.0, 200.0);
    s.deliver(18.0);
    let (_, r) = s.job("tyres");
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { .. } | ReviewReason::FastAverageGame { .. })));
}

#[test]
fn the_in_game_pace_check_uses_the_truckersmp_clock() {
    // On TruckersMP one in-game minute is 10 real seconds (6 per real minute),
    // not the map scale. 75 mph at road scale 20 would read 6.7 "km per minute"
    // if TruckersMP game minutes were treated like singleplayer ones.
    let mut s = Sim::new("ats");
    s.truckersmp_sdk(true);
    s.time_rate = Some(6.0);
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(1800.0, 75.0);
    s.deliver(1207.0);
    let (_, r) = s.job("tyres");
    assert!(r.truckersmp);
    assert!(!r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { .. } | ReviewReason::FastAverageGame { .. })), "{:?}", r.review);

    // A TruckersMP speed hack still trips both.
    let mut s = Sim::new("ats");
    s.truckersmp_sdk(true);
    s.time_rate = Some(6.0);
    s.accept(JobSpec::new("tyres"));
    s.drive_secs(240.0, 200.0); // 24 in-game minutes at the TruckersMP rate
    s.deliver(429.0);
    let (_, r) = s.job("tyres");
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::FastAverage { .. })), "{:?}", r.review);
    assert!(r.review.iter().any(|x| matches!(x, ReviewReason::FastAverageGame { .. })), "{:?}", r.review);
}

// ---- bounded memory (PLAN §7a) -------------------------------------------------

/// Delivers a job, then the game restarts `hours_later` into a save taken
/// during that job and drives on.
fn restart_into_delivered_job(hours_later: f64) -> Sim {
    let mut s = Sim::new("ats");
    s.accept(JobSpec::new("tyres"));
    s.drive_km(10.0);
    let mid = s.save();
    s.drive_km(10.0);
    s.deliver(20.0);
    s.wall_ms += (hours_later * 3_600_000.0) as i64;
    s.restart(Some(&mid));
    s.drive_km(10.0);
    s
}

#[test]
fn a_delivered_job_stays_reopenable_inside_the_window() {
    let s = restart_into_delivered_job(47.0);
    let jobs = s.replay();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job.state, JobState::Stopped);
}

#[test]
fn a_job_finished_before_the_window_is_evicted_and_a_late_load_is_reviewed() {
    let s = restart_into_delivered_job(49.0);
    let cfg = pt_replay::sample_config();
    let (frames, _) = pt_replay::parse_lines(&s.lines, &cfg);
    let mut profiles = pt_core::timeline::Profiles::new(cfg.clone());
    let mut evicted = Vec::new();
    for f in &frames {
        profiles.push(f);
        evicted.extend(profiles.take_evicted());
    }
    assert_eq!(evicted.len(), 1, "evicted when the new session starts");
    assert_eq!(evicted[0].state, JobState::Delivered);
    let live: Vec<_> = profiles.jobs().collect();
    assert_eq!(live.len(), 1);
    assert_eq!((live[0].state, live[0].start), (JobState::Tracking, StartKind::LateStart));
    assert_ne!(live[0].id, evicted[0].id, "ids stay unique after eviction");
    // A full replay sees the same two jobs.
    let jobs = s.replay();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].result.outcome, Outcome::Valid, "{:?}", jobs[0].result.review);
}
