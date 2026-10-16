//! Scoring (PLAN §6), evaluated on a job's kept timeline.
//!
//! Distances (decided and measured 2026-09-28):
//! - Only loaded, hitched driving counts; World of Trucks jobs count everything.
//! - The game's `job.delivered` `distance.km` is game scale, whole km, includes
//!   the empty leg and comes from the save. It is cross-checked against the
//!   tracked whole-job odometer distance; the uncounted legs are then subtracted
//!   and the rest converted to 1:1 with the job's own world/game ratio.

use serde::{Deserialize, Serialize};

use crate::config::{Config, ReviewAction, ScaleClass};
use crate::job::JobInfo;
use crate::timeline::{Job, Point, StartKind};
use crate::units::{exceeds, km_to_mi};

/// A bonus-cargo entry (server-side table, PLAN §7 `bonus_cargo`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bonus {
    /// Server id (`bon_…`); empty in tests.
    #[serde(default)]
    pub id: String,
    pub game: crate::game::Game,
    /// Matched against `cargo.id`, ignoring case: equal when `exact`, else a glob
    /// where `*` is any run of characters and `?` one character.
    pub cargo_match: String,
    #[serde(default)]
    pub exact: bool,
    pub kind: BonusKind,
    pub amount: f64,
    /// Half-open window `[active_from_ms, active_to_ms)` on the server's
    /// delivery time.
    pub active_from_ms: Option<i64>,
    pub active_to_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BonusKind {
    /// Points added once.
    Flat,
    /// Points multiplied.
    Multiplier,
}

/// Facts from outside the trace.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub bonuses: Vec<Bonus>,
    /// Rule 0 (server): config and install manifest matched a published version.
    /// `None` = not checked (client-side provisional result).
    pub integrity_ok: Option<bool>,
    pub singleplayer_division: bool,
    /// Server clock at delivery, for bonus windows.
    pub delivered_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Not delivered (yet): tracking, suspended, cancelled or stopped.
    Incomplete,
    Discarded,
    Invalid,
    PendingReview,
    Valid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    Integrity,
    NegativeDistance,
    SingleplayerDivisionTruckersmp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ReviewReason {
    /// Rule 3.
    LongJob { world_km: f64 },
    /// The whole job averaged more than the threshold in 1:1 km per real
    /// (unpaused) minute: a speed hack or a boosted truck.
    FastAverage { world_km: f64, driving_min: f64, world_km_per_min: f64 },
    /// The same timed by the game's clock (in-game minutes converted back to
    /// real minutes at the game's time rate).
    FastAverageGame { world_km: f64, game_clock_min: f64, world_km_per_min: f64 },
    /// Rule 4: fuel used below −tolerance in a tracked run.
    NegativeFuel { run: u32, used_l: f64 },
    /// Rule 4: fuel rose outside a detected refuel.
    FuelJump { wall_ms: i64, litres: f64 },
    Gap { count: usize },
    LateStart,
    CargoSwitch,
    /// The game's delivery distance disagrees with the trace.
    DistanceMismatch { delivered_km: f64, tracked_km: f64, tolerance_km: f64 },
    NoDeliveredDistance,
    /// The delivery paid more per mile of the game's delivery distance than
    /// `review_over_income_per_game_mi`, or more in all than `review_over_income`.
    HighIncome { income: i64, game_mi: f64, per_game_mi: f64 },
    /// The game ran faster than real time (the `warp` console command): the
    /// fastest 30 s of real time, and the game seconds gained over all of them.
    TimeWarp { max_factor: f64, gained_s: f64 },
}

impl ReviewReason {
    /// The stable code (docs/ADMIN_API.md §2), the same as the serde tag.
    pub fn code(&self) -> &'static str {
        match self {
            ReviewReason::LongJob { .. } => "long_job",
            ReviewReason::FastAverage { .. } => "fast_average",
            ReviewReason::FastAverageGame { .. } => "fast_average_game",
            ReviewReason::NegativeFuel { .. } => "negative_fuel",
            ReviewReason::FuelJump { .. } => "fuel_jump",
            ReviewReason::Gap { .. } => "gap",
            ReviewReason::LateStart => "late_start",
            ReviewReason::CargoSwitch => "cargo_switch",
            ReviewReason::DistanceMismatch { .. } => "distance_mismatch",
            ReviewReason::NoDeliveredDistance => "no_delivered_distance",
            ReviewReason::TimeWarp { .. } => "time_warp",
            ReviewReason::HighIncome { .. } => "high_income",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeedStats {
    pub peak_mph: f64,
    pub over_penalty_s: f64,
    pub over_invalid_s: f64,
    /// Rule 5.
    pub penalty: bool,
    /// Rule 2.
    pub invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refuel {
    pub wall_ms: i64,
    pub litres: f64,
    pub duration_s: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Distances {
    /// Tracked, whole job (accept → delivery, kept timeline + gaps), game km.
    pub tracked_whole_game_km: f64,
    /// Tracked counted (loaded + hitched, or everything for WoT), game km.
    pub tracked_counted_game_km: f64,
    pub tracked_uncounted_game_km: f64,
    /// Tracked counted, 1:1 km (Σ Δodometer ÷ local.scale).
    pub tracked_counted_world_km: f64,
    /// Driven (kept, not gap) counted distance: the basis of the 1:1 ratio.
    pub driven_counted_game_km: f64,
    pub driven_counted_world_km: f64,
    /// The game's figure (`job.delivered` `distance.km`).
    pub delivered_game_km: Option<f64>,
    /// Scored game km: delivered − uncounted (or tracked counted).
    pub counted_game_km: f64,
    /// Scored 1:1 km.
    pub world_km: f64,
    pub world_mi: f64,
    /// Shown next to the points: counted distance in the game's own scale.
    pub game_mi: f64,
    /// Counted game km per scale class (route map colouring and admin detail).
    pub game_km_by_scale: Vec<(ScaleClass, f64)>,
    /// The same in 1:1 km.
    #[serde(default)]
    pub world_km_by_scale: Vec<(ScaleClass, f64)>,
    /// Whole job, driven only (no gaps): 1:1 km and unpaused simulation
    /// seconds, for the average-pace check.
    pub driven_whole_world_km: f64,
    pub driven_s: f64,
    /// Whole job, driven only: in-game minutes elapsed, and the same turned
    /// back into real minutes at the game's time rate.
    pub driven_game_min: f64,
    pub driven_game_clock_min: f64,
    /// Negative kept-timeline odometer movement (rule 1).
    pub negative_km: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobResult {
    pub outcome: Outcome,
    pub discard: Option<DiscardReason>,
    pub review: Vec<ReviewReason>,
    pub distances: Distances,
    pub speed: SpeedStats,
    pub refuels: Vec<Refuel>,
    /// Accepted 1:1 miles after the rule 5 penalty.
    pub accepted_world_mi: f64,
    pub bonus: Option<Bonus>,
    /// Points before rounding.
    pub points_raw: f64,
    /// Final points (floored to `points_decimals`); 0 when discarded or invalid.
    pub points: f64,
    pub truckersmp: bool,
    pub convoy: bool,
}

fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat.len() == s.len() && pat.chars().zip(s.chars()).all(|(p, c)| p == '?' || p == c),
        Some((pre, rest)) => {
            if s.len() < pre.len() || !s.is_char_boundary(pre.len()) || !glob(pre, &s[..pre.len()]) {
                return false;
            }
            let tail = &s[pre.len()..];
            (0..=tail.len()).any(|i| tail.is_char_boundary(i) && glob(rest, &tail[i..]))
        }
    }
}

/// The `warp` console command (0.0001-60) runs every game clock faster than
/// real time: simulation, paused simulation, render and the in-game clock (the
/// owner's ATS test, 2026-10-16). The truck's speed and the pace checks look
/// normal, so only the player's PC clock (`wall_ms`) shows it. Game time is
/// compared with real time over windows of this much real driving.
pub const TIME_WARP_WINDOW_S: f64 = 30.0;
/// A window whose game time ran more than this much faster than real time is
/// warped. Honest play stays within 1.047 (38 sessions, both games, capture
/// mode included); `warp 2` reads 2.0.
pub const TIME_WARP_OVER: f64 = 1.15;
/// Pairs further apart in real time are a pause or a stall, and are left out
/// (they would only lower the ratio, so pausing cannot hide a warp).
const TIME_WARP_MAX_STEP_S: f64 = 5.0;

/// The fastest warped window and the game seconds gained in all of them.
fn time_warp(kept: &[Point]) -> Option<(f64, f64)> {
    let (mut sim, mut wall, mut max, mut gained) = (0.0, 0.0, 0.0f64, 0.0);
    for (a, b) in pairs(kept) {
        let ds = b.sim_us.saturating_sub(a.sim_us) as f64 / 1e6;
        let dw = (b.wall_ms - a.wall_ms) as f64 / 1000.0;
        if ds <= 0.0 || ds > 60.0 || dw <= 0.0 || dw >= TIME_WARP_MAX_STEP_S {
            continue;
        }
        sim += ds;
        wall += dw;
        if wall >= TIME_WARP_WINDOW_S {
            let factor = sim / wall;
            if factor > TIME_WARP_OVER {
                max = max.max(factor);
                gained += sim - wall;
            }
            (sim, wall) = (0.0, 0.0);
        }
    }
    (max > 0.0).then_some((max, gained))
}

/// Neighbouring kept points of the same run: the only pairs that accumulate.
fn pairs(kept: &[Point]) -> impl Iterator<Item = (&Point, &Point)> {
    kept.windows(2).filter(|w| w[0].run == w[1].run && w[0].truck == w[1].truck).map(|w| (&w[0], &w[1]))
}

fn distances(job: &Job, cfg: &Config) -> Distances {
    let wot = job.info.is_world_of_trucks();
    let vanilla = cfg.map.scale.vanilla(job.game);
    let eps = cfg.rules.distance.odometer_epsilon_km;
    let mut d = Distances::default();
    let mut by_scale: Vec<(ScaleClass, f64)> = Vec::new();
    let mut world_by_scale: Vec<(ScaleClass, f64)> = Vec::new();
    let mut add = |d: &mut Distances, km: f64, counted: bool, scale: f64| {
        d.tracked_whole_game_km += km;
        if counted || wot {
            d.tracked_counted_game_km += km;
            d.tracked_counted_world_km += km / scale;
            let class = vanilla.classify(Some(scale));
            for (list, v) in [(&mut by_scale, km), (&mut world_by_scale, km / scale)] {
                match list.iter_mut().find(|(c, _)| *c == class) {
                    Some((_, x)) => *x += v,
                    None => list.push((class, v)),
                }
            }
        } else {
            d.tracked_uncounted_game_km += km;
        }
    };
    for (a, b) in pairs(&job.kept) {
        let (Some(x), Some(y)) = (a.odometer_km, b.odometer_km) else { continue };
        let km = y - x;
        if km < -eps {
            d.negative_km += -km;
            continue;
        }
        d.driven_whole_world_km += km.max(0.0) / b.local_scale;
        d.driven_s += b.sim_us.saturating_sub(a.sim_us) as f64 / 1e6;
        if let (Some(ta), Some(tb)) = (a.game_time_min, b.game_time_min) {
            let game_min = tb.saturating_sub(ta) as f64;
            // Singleplayer time runs at local.scale; TruckersMP at its server rate.
            let rate = if b.truckersmp { cfg.rules.truckersmp_game_min_per_real_min } else { b.local_scale };
            d.driven_game_min += game_min;
            d.driven_game_clock_min += game_min / rate;
        }
        let counted = b.cargo_loaded && b.trailer_connected;
        if counted || wot {
            d.driven_counted_game_km += km.max(0.0);
            d.driven_counted_world_km += km.max(0.0) / b.local_scale;
        }
        add(&mut d, km.max(0.0), counted, b.local_scale);
    }
    for g in job.active_gaps() {
        if let Some(km) = g.game_km.filter(|k| *k > 0.0) {
            add(&mut d, km, g.counted, g.local_scale);
        }
    }
    d.game_km_by_scale = by_scale;
    d.world_km_by_scale = world_by_scale;
    d
}

fn speed(job: &Job, cfg: &Config) -> SpeedStats {
    let (penalty_mph, invalid_mph) = cfg.rules.speed.limits_for(job.game, job.info.fingerprint.route.job_type);
    let prec = cfg.rules.speed.compare_precision_mph;
    let window = cfg.rules.speed.sustained_window_s;
    let mut s = SpeedStats::default();
    // Episodes: consecutive over-limit points of one run, timed in paused
    // simulation time, so pausing neither adds time nor splits an episode.
    let mut ep_pen: Option<u64> = None;
    let mut ep_inv: Option<u64> = None;
    let mut prev: Option<&Point> = None;
    for p in &job.kept {
        s.peak_mph = s.peak_mph.max(p.mph());
        if prev.is_some_and(|q| q.run != p.run) || p.after_teleport {
            ep_pen = None;
            ep_inv = None;
        }
        if let Some(q) = prev.filter(|q| q.run == p.run && !p.after_teleport) {
            let dt = p.sim_us.saturating_sub(q.sim_us) as f64 / 1e6;
            if exceeds(q.mph(), penalty_mph, prec) && exceeds(p.mph(), penalty_mph, prec) {
                s.over_penalty_s += dt;
            }
            if exceeds(q.mph(), invalid_mph, prec) && exceeds(p.mph(), invalid_mph, prec) {
                s.over_invalid_s += dt;
            }
        }
        for (limit, ep, flag) in [
            (penalty_mph, &mut ep_pen, &mut s.penalty),
            (invalid_mph, &mut ep_inv, &mut s.invalid),
        ] {
            if exceeds(p.mph(), limit, prec) {
                let since = *ep.get_or_insert(p.sim_us);
                if p.sim_us.saturating_sub(since) as f64 / 1e6 > window {
                    *flag = true;
                }
            } else {
                *ep = None;
            }
        }
        prev = Some(p);
    }
    s
}

/// Refuel episodes and rule 4 per run.
fn fuel(job: &Job, cfg: &Config, review: &mut Vec<ReviewReason>) -> Vec<Refuel> {
    let r = &cfg.rules.refuel;
    let f = &cfg.rules.fuel;
    let mut refuels = Vec::new();
    let mut runs: Vec<&[Point]> = Vec::new();
    let mut start = 0;
    for i in 1..=job.kept.len() {
        if i == job.kept.len() || job.kept[i].run != job.kept[i - 1].run || job.kept[i].truck != job.kept[i - 1].truck {
            runs.push(&job.kept[start..i]);
            start = i;
        }
    }
    for run in runs {
        // Fuel rises while stationary, in steps (every 0.3 s of simulation in
        // ATS 1.61), so zero-change samples inside an episode are allowed.
        let mut refuelled = 0.0;
        let mut in_refuel = vec![false; run.len()];
        let mut i = 1;
        while i < run.len() {
            let rising = |k: usize| match (run[k - 1].fuel_l, run[k].fuel_l) {
                (Some(a), Some(b)) => b - a > 1e-6,
                _ => false,
            };
            let still = |k: usize| run[k].mph() < r.stationary_below_mph;
            if !(rising(i) && still(i)) {
                i += 1;
                continue;
            }
            let first = i;
            let mut last = i;
            let mut k = i + 1;
            while k < run.len() && still(k) && run[k].sim_us.saturating_sub(run[last].sim_us) <= 2_000_000 {
                if rising(k) {
                    last = k;
                }
                k += 1;
            }
            let litres = run[last].fuel_l.unwrap_or(0.0) - run[first - 1].fuel_l.unwrap_or(0.0);
            let dur = run[last].sim_us.saturating_sub(run[first - 1].sim_us) as f64 / 1e6;
            let rate = if dur > 0.0 { litres / dur } else { f64::INFINITY };
            if dur >= r.min_duration_s && rate >= r.min_rate_l_per_s && rate <= r.max_rate_l_per_s {
                refuelled += litres;
                refuels.push(Refuel { wall_ms: run[first].wall_ms, litres, duration_s: dur });
                for flag in &mut in_refuel[first..=last] {
                    *flag = true;
                }
            }
            i = last + 1;
        }
        // Fuel that changes across a teleport is ignored, like across a load:
        // World of Trucks event portals (Brackenreach, owner 2026-10-15) fill
        // the tank as they move the truck.
        let mut teleported = 0.0;
        for k in 1..run.len() {
            if let (Some(a), Some(b)) = (run[k - 1].fuel_l, run[k].fuel_l) {
                if run[k].after_teleport {
                    teleported += b - a;
                } else if !in_refuel[k] && b - a > f.mid_job_jump_tolerance_l {
                    review.push(ReviewReason::FuelJump { wall_ms: run[k].wall_ms, litres: b - a });
                }
            }
        }
        if let (Some(a), Some(b)) = (run.first().and_then(|p| p.fuel_l), run.last().and_then(|p| p.fuel_l)) {
            let used = a - b + refuelled + teleported;
            if used < -f.negative_fuel_tolerance_l {
                review.push(ReviewReason::NegativeFuel { run: run[0].run, used_l: used });
            }
        }
    }
    refuels
}

fn pick_bonus(info: &JobInfo, base: f64, ctx: &Context) -> Option<(Bonus, f64)> {
    let cargo = &info.fingerprint.cargo.cargo_id;
    let game = info.fingerprint.route.game;
    ctx.bonuses
        .iter()
        .filter(|b| {
            b.game == game
                && if b.exact {
                    b.cargo_match.eq_ignore_ascii_case(cargo)
                } else {
                    glob(&b.cargo_match.to_ascii_lowercase(), &cargo.to_ascii_lowercase())
                }
        })
        .filter(|b| match ctx.delivered_at_ms {
            Some(t) => b.active_from_ms.is_none_or(|from| from <= t) && b.active_to_ms.is_none_or(|to| t < to),
            None => true,
        })
        .map(|b| {
            let pts = match b.kind {
                BonusKind::Flat => base + b.amount,
                BonusKind::Multiplier => base * b.amount,
            };
            (b.clone(), pts)
        })
        .fold(None, |best: Option<(Bonus, f64)>, c| match best {
            Some(b) if b.1 >= c.1 => Some(b),
            _ => Some(c),
        })
}

/// Scores a job. Works on any state; only a delivered job gets an outcome
/// other than `Incomplete`.
pub fn evaluate(job: &Job, cfg: &Config, ctx: &Context) -> JobResult {
    let r = &cfg.rules;
    let mut review = Vec::new();
    let mut d = distances(job, cfg);
    let speed = speed(job, cfg);
    let refuels = fuel(job, cfg, &mut review);
    let truckersmp = job.kept.iter().any(|p| p.truckersmp);
    // TruckersMP also reports a multiplayer time offset (2026-09-28 capture), so a
    // non-zero offset means Convoy only when TruckersMP is not loaded.
    let convoy = !truckersmp && job.kept.iter().any(|p| p.mp_time_offset != 0);

    // Distance: the game's figure, cross-checked, minus uncounted legs, to 1:1.
    d.delivered_game_km = job.delivered.as_ref().and_then(|x| x.distance_km);
    // The job's own mix of city and road scale, from driving only: a gap (a
    // forward load, possibly a save edit) carries just one scale reading.
    let ratio = if d.driven_counted_game_km > 0.0 {
        d.driven_counted_world_km / d.driven_counted_game_km
    } else {
        1.0 / cfg.map.scale.vanilla(job.game).main_road_local_scale()
    };
    match d.delivered_game_km {
        Some(delivered) => {
            let tol_km = r.distance.delivered_vs_trace_tolerance_mi(km_to_mi(delivered)) / crate::units::MI_PER_KM
                + r.distance.delivered_distance_resolution_km;
            let off = match job.start {
                // Only the part that was seen can be checked: the game may know more.
                StartKind::LateStart => (d.tracked_whole_game_km - delivered).max(0.0),
                StartKind::Tracked => (delivered - d.tracked_whole_game_km).abs(),
            };
            if off > tol_km && cfg.review_policy.trace_checks == ReviewAction::Review {
                review.push(ReviewReason::DistanceMismatch {
                    delivered_km: delivered,
                    tracked_km: d.tracked_whole_game_km,
                    tolerance_km: tol_km,
                });
            }
            d.counted_game_km = if job.info.is_world_of_trucks() {
                delivered
            } else {
                (delivered - d.tracked_uncounted_game_km).max(0.0)
            };
        }
        None => {
            if job.delivered.is_some() {
                review.push(ReviewReason::NoDeliveredDistance);
            }
            d.counted_game_km = d.tracked_counted_game_km;
        }
    }
    d.world_km = d.counted_game_km * ratio;
    d.world_mi = km_to_mi(d.world_km);
    d.game_mi = km_to_mi(d.counted_game_km);

    // Review reasons from the timeline and rule 3.
    let policy = &cfg.review_policy;
    let gaps = job
        .active_gaps()
        .filter(|g| g.kind == crate::timeline::GapKind::Load)
        .filter(|g| {
            let mi = km_to_mi(g.game_km.unwrap_or(f64::INFINITY).abs());
            !(mi < policy.min_gap_distance_mi && g.duration_s < policy.min_gap_duration_s)
        })
        .count();
    if gaps > 0 && policy.gap == ReviewAction::Review {
        review.push(ReviewReason::Gap { count: gaps });
    }
    if job.start == StartKind::LateStart && policy.late_start == ReviewAction::Review {
        review.push(ReviewReason::LateStart);
    }
    if job.cargo_switched {
        review.push(ReviewReason::CargoSwitch);
    }
    if exceeds(d.world_km, r.manual_review_over_world_km, r.distance.compare_precision_km) {
        review.push(ReviewReason::LongJob { world_km: d.world_km });
    }
    if let Some(income) = job.delivered.as_ref().and_then(|x| x.revenue) {
        let game_mi = km_to_mi(d.delivered_game_km.unwrap_or(d.tracked_whole_game_km));
        let per_game_mi = if game_mi > 0.0 { income as f64 / game_mi } else { 0.0 };
        let per_mile = r.review_over_income_per_game_mi.is_some_and(|limit| game_mi > 0.0 && per_game_mi > limit);
        let total = r.review_over_income.is_some_and(|limit| income as f64 > limit);
        if per_mile || total {
            review.push(ReviewReason::HighIncome { income, game_mi, per_game_mi });
        }
    }
    if d.driven_s >= r.average_pace_min_driving_s {
        let minutes = d.driven_s / 60.0;
        let pace = d.driven_whole_world_km / minutes;
        if pace > r.review_over_average_world_km_per_min {
            review.push(ReviewReason::FastAverage {
                world_km: d.driven_whole_world_km,
                driving_min: minutes,
                world_km_per_min: pace,
            });
        }
    }
    if d.driven_game_min >= r.average_pace_min_game_min && d.driven_game_clock_min > 0.0 {
        let pace = d.driven_whole_world_km / d.driven_game_clock_min;
        if pace > r.review_over_average_world_km_per_game_clock_min {
            review.push(ReviewReason::FastAverageGame {
                world_km: d.driven_whole_world_km,
                game_clock_min: d.driven_game_clock_min,
                world_km_per_min: pace,
            });
        }
    }
    if policy.trace_checks == ReviewAction::Review {
        if let Some((max_factor, gained_s)) = time_warp(&job.kept) {
            review.push(ReviewReason::TimeWarp { max_factor, gained_s });
        }
    }

    // Points: rules 5, 6, 7 (bonus on the cargo first tracked after a switch).
    let accepted = if speed.penalty { d.world_mi * r.speed_penalty_factor } else { d.world_mi };
    let base = accepted * r.points_per_world_mi;
    let (bonus, points_raw) = match pick_bonus(&job.first_info, base, ctx) {
        Some((b, p)) => (Some(b), p),
        None => (None, base),
    };

    let discard = if ctx.integrity_ok == Some(false) {
        Some(DiscardReason::Integrity)
    } else if d.negative_km > r.distance.odometer_epsilon_km {
        Some(DiscardReason::NegativeDistance)
    } else if ctx.singleplayer_division && truckersmp {
        Some(DiscardReason::SingleplayerDivisionTruckersmp)
    } else {
        None
    };
    // A delivered job keeps its result even when a later load stops its tracking.
    let outcome = if job.delivered.is_none() {
        Outcome::Incomplete
    } else if discard.is_some() {
        Outcome::Discarded
    } else if speed.invalid {
        Outcome::Invalid
    } else if !review.is_empty() {
        Outcome::PendingReview
    } else {
        Outcome::Valid
    };
    let points = match outcome {
        Outcome::Discarded | Outcome::Invalid => 0.0,
        _ => r.floor_points(points_raw),
    };
    JobResult {
        outcome,
        discard,
        review,
        distances: d,
        speed,
        refuels,
        accepted_world_mi: accepted,
        bonus,
        points_raw,
        points,
        truckersmp,
        convoy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob("dynamite", "dynamite"));
        assert!(glob("dyna*", "dynamite"));
        assert!(glob("*mite", "dynamite"));
        assert!(glob("d*n*e", "dynamite"));
        assert!(!glob("dyna", "dynamite"));
        assert!(!glob("x*", "dynamite"));
        assert!(glob("dyn?mite", "dynamite"));
        assert!(glob("d?n*", "dynamite"));
        assert!(!glob("dyn?", "dynamite"));
    }

    #[test]
    fn review_codes_match_the_serialised_tag() {
        let all = [
            ReviewReason::LongJob { world_km: 1.0 },
            ReviewReason::FastAverage { world_km: 1.0, driving_min: 1.0, world_km_per_min: 1.0 },
            ReviewReason::FastAverageGame { world_km: 1.0, game_clock_min: 1.0, world_km_per_min: 1.0 },
            ReviewReason::NegativeFuel { run: 1, used_l: -1.0 },
            ReviewReason::FuelJump { wall_ms: 1, litres: 1.0 },
            ReviewReason::Gap { count: 1 },
            ReviewReason::LateStart,
            ReviewReason::CargoSwitch,
            ReviewReason::DistanceMismatch { delivered_km: 1.0, tracked_km: 1.0, tolerance_km: 1.0 },
            ReviewReason::NoDeliveredDistance,
            ReviewReason::TimeWarp { max_factor: 2.0, gained_s: 30.0 },
            ReviewReason::HighIncome { income: 1, game_mi: 1.0, per_game_mi: 1.0 },
        ];
        for r in all {
            assert_eq!(serde_json::to_value(&r).unwrap()["reason"], r.code());
        }
    }
}
