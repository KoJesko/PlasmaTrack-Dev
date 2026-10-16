//! Job timeline reconstruction (PLAN §5).
//!
//! Frames of one (player, game, profile) stream are pushed in plugin order. The
//! timeline follows the game's own history: a save load truncates, restores or
//! gaps the job's *kept* timeline, and reverted material is kept separately and
//! never scored. The whole design rests on the capture findings in PLAN §3:
//!
//! - **Bracket**: everything between two consecutive samples. Loads, deliveries,
//!   config changes and teleports all happen inside one.
//! - **W (world reload)**: `job` and `truck` configs both emptied in one bracket.
//!   All captured in-session loads showed it; accepts, cargo loading, cancels and
//!   ferries never empty `truck`.
//! - **S (session change)**: new plugin session, seq gap or `timer_restart`. It is
//!   a load only together with a discontinuity against the last kept state.
//! - Anything else that jumps is a **teleport**: no distance, no load, no review.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::game::Game;
use crate::job::{Delivered, Fingerprint, JobInfo, JobType};
use crate::trace::{attr, Body, Frame, Sample};
use crate::units::mps_to_mph;

/// Plausible top speed used to tell a data gap from driving (world m/s).
const MAX_PLAUSIBLE_WORLD_MPS: f64 = 100.0;

/// One kept (or reverted) sample of a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub sess: u32,
    pub seq: u64,
    pub wall_ms: i64,
    /// Paused simulation time (µs); only comparable within one run.
    pub sim_us: u64,
    /// Run id: distance and overspeed only accumulate between neighbours of the
    /// same run. A new run starts at every load, gap, session change, truck change
    /// and teleport.
    pub run: u32,
    pub speed_mps: f64,
    pub odometer_km: Option<f64>,
    pub fuel_l: Option<f64>,
    pub pos: Option<[f64; 3]>,
    pub game_time_min: Option<u32>,
    /// `local.scale` (1 when the game does not provide it: a 1:1 map).
    pub local_scale: f64,
    pub cargo_loaded: bool,
    pub trailer_connected: bool,
    pub truck: u32,
    pub mp_time_offset: i64,
    pub truckersmp: bool,
    /// First point after a teleport (ferry, train, F7): overspeed episodes end
    /// here, but distance carries on (the odometer does not move on a teleport).
    #[serde(default)]
    pub after_teleport: bool,
    /// Cargo damage 0-1 when the game reports it (job graphs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_damage: Option<f64>,
}

impl Point {
    pub fn mph(&self) -> f64 {
        mps_to_mph(self.speed_mps.abs())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Tracking,
    Suspended,
    Delivered,
    Cancelled,
    /// A save of this already-delivered job was loaded; nothing is recorded.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    Tracked,
    LateStart,
}

/// A stretch of a job that a save load undid. Never scored unless a later
/// load returns to it ("revert, then restore").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    /// Seq of the kept point this branch forked from.
    pub fork_seq: u64,
    pub points: Vec<Point>,
    pub reverted_at_wall_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// A forward load to a point never seen: goes to review.
    Load,
    /// Missing samples (client closed, game restarted) with plausible driving in
    /// between: the distance counts and nothing is reviewed.
    Bridge,
}

/// Distance between two kept points that are not neighbours on one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub kind: GapKind,
    pub from_seq: u64,
    pub to_sess: u32,
    pub to_seq: u64,
    /// Odometer difference when both ends are the same truck (game km).
    pub game_km: Option<f64>,
    pub local_scale: f64,
    pub counted: bool,
    pub duration_s: f64,
    pub wall_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEvent {
    Started { start: StartKind, wall_ms: i64 },
    /// A load truncated the kept timeline back to `to_seq`.
    LoadTruncate { to_seq: u64, reverted_points: usize, wall_ms: i64 },
    /// A load returned into previously reverted material, which is restored.
    LoadRestore { from_fork_seq: u64, restored_points: usize, wall_ms: i64 },
    LoadGap { game_km: Option<f64>, wall_ms: i64 },
    /// A session change with no discontinuity: the job simply continues.
    SessionContinue { wall_ms: i64 },
    Teleport {
        world_m: f64,
        ferry_or_train: bool,
        /// `ferry` or `train` when the game said which.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
        wall_ms: i64,
    },
    Suspended { wall_ms: i64 },
    Resumed { wall_ms: i64 },
    CargoLoaded { wall_ms: i64 },
    CargoSwitch { from: Box<crate::job::CargoKey>, to: Box<crate::job::CargoKey>, wall_ms: i64 },
    TruckChange { wall_ms: i64 },
    Delivered { wall_ms: i64 },
    Cancelled { penalty: Option<i64>, wall_ms: i64 },
    CancellationReverted { wall_ms: i64 },
    /// A load reopened a delivery that had been Invalid (falling under the map).
    InvalidDeliveryReopened { wall_ms: i64 },
    /// A save of this delivered job was loaded; tracking stopped.
    StoppedAfterDelivery { wall_ms: i64 },
    /// The job was delivered again from an old save: ignored.
    DuplicateDelivery { wall_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u32,
    pub game: Game,
    pub profile: String,
    /// Latest job config.
    pub info: JobInfo,
    /// The config the job was first tracked with (bonus basis after a cargo switch).
    pub first_info: JobInfo,
    pub state: JobState,
    pub start: StartKind,
    pub kept: Vec<Point>,
    pub reverted: Vec<Branch>,
    pub gaps: Vec<Gap>,
    pub events: Vec<JobEvent>,
    pub cargo_switched: bool,
    pub delivered: Option<Delivered>,
    pub delivered_wall_ms: Option<i64>,
    /// Deliveries undone by a load (Invalid deliveries reopened).
    pub reverted_deliveries: Vec<Delivered>,
}

impl Job {
    /// Gaps whose far end is still on the kept timeline. A later load back past
    /// a gap reverts it together with everything after it.
    pub fn active_gaps(&self) -> impl Iterator<Item = &Gap> {
        self.gaps.iter().filter(|g| self.kept.iter().any(|p| p.sess == g.to_sess && p.seq == g.to_seq))
    }
}

/// Streaming notices for the client (provisional; the server re-derives all).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    JobStarted { job: u32 },
    JobDelivered { job: u32 },
    JobCancelled { job: u32 },
    /// Rule 5's speed penalty became active on the kept timeline.
    PenaltyActivated { job: u32, wall_ms: i64 },
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Bracket {
    /// New plugin session or a seq gap: the plugin re-sends empty configs at
    /// init, so W is not evidence here; only a discontinuity is.
    session_change: bool,
    timer_restart: bool,
    job_emptied: bool,
    truck_emptied: bool,
    truck_changed: bool,
    transfer: bool,
    /// `ferry` or `train`, from the gameplay event.
    transfer_kind: Option<String>,
    /// The game sent a truck while no job was set: the state it is in (after a
    /// load, a cancel or a delivery) has no job.
    no_job_state: bool,
    /// A job config arrived after that: the job was accepted in this bracket,
    /// so its start was seen (quick job taken right after loading, 2026-10-13).
    job_after_no_job: bool,
}

/// Streaming overspeed state for the penalty notification.
#[derive(Default, Serialize, Deserialize)]
struct OverspeedWatch {
    run: u32,
    since_us: Option<u64>,
    notified: bool,
}

/// The state is serialisable so the server can snapshot it between batches
/// (PLAN §7a); a restored timeline continues exactly like the original.
#[derive(Serialize, Deserialize)]
pub struct Timeline {
    cfg: Config,
    profile: String,
    game: Option<Game>,
    jobs: Vec<Job>,
    current: Option<usize>,
    sess_ids: Vec<String>,
    sess: Option<u32>,
    last_seq: u64,
    bracket: Bracket,
    /// The current job: freight, else a road trip.
    job_cfg: Option<JobInfo>,
    #[serde(default)]
    freight_cfg: Option<JobInfo>,
    #[serde(default)]
    car_cfg: Option<JobInfo>,
    truck_ids: Vec<String>,
    truck: u32,
    /// Loaded-module heuristic (fallback for plugins without the TruckersMP SDK side).
    truckersmp_module: bool,
    /// TruckersMP SDK state, once reported in this session.
    truckersmp_sdk: Option<bool>,
    truckersmp: bool,
    last: Option<Point>,
    next_run: u32,
    watch: Vec<OverspeedWatch>,
    notices: Vec<Notice>,
    /// Next job id. Ids stay unique when old jobs are evicted.
    next_id: u32,
    /// Jobs that left the live timeline (`reopen_window_wall_h`), not yet taken.
    evicted: Vec<Job>,
}

impl Timeline {
    pub fn new(cfg: Config, profile: impl Into<String>) -> Timeline {
        Timeline {
            cfg,
            profile: profile.into(),
            game: None,
            jobs: Vec::new(),
            current: None,
            sess_ids: Vec::new(),
            sess: None,
            last_seq: 0,
            bracket: Bracket::default(),
            job_cfg: None,
            freight_cfg: None,
            car_cfg: None,
            truck_ids: Vec::new(),
            truck: 0,
            truckersmp_module: false,
            truckersmp_sdk: None,
            truckersmp: false,
            last: None,
            next_run: 0,
            watch: Vec::new(),
            notices: Vec::new(),
            next_id: 0,
            evicted: Vec::new(),
        }
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// Every job, evicted ones included, in creation order.
    pub fn into_jobs(self) -> Vec<Job> {
        let mut jobs = self.evicted;
        jobs.extend(self.jobs);
        jobs.sort_by_key(|j| j.id);
        jobs
    }

    /// Jobs evicted since the last call (PLAN §7a). They never change again.
    pub fn take_evicted(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.evicted)
    }

    /// Drops jobs that ended more than `reopen_window_wall_h` before `now_wall`
    /// (a frame's wall clock, so a full replay evicts exactly the same jobs).
    /// Delivered, cancelled and stopped jobs end at their last event; a suspended
    /// job counts as abandoned after the same time. The current job always stays.
    fn evict_finished(&mut self, now_wall: i64) {
        let window_ms = (self.cfg.timeline.reopen_window_wall_h * 3_600_000.0) as i64;
        let mut k = self.jobs.len();
        while k > 0 {
            k -= 1;
            let job = &self.jobs[k];
            let ended = matches!(
                job.state,
                JobState::Delivered | JobState::Cancelled | JobState::Stopped | JobState::Suspended
            );
            let last = job.events.last().map(event_wall).unwrap_or(0);
            if self.current == Some(k) || !ended || now_wall - last <= window_ms {
                continue;
            }
            self.evicted.push(self.jobs.remove(k));
            self.watch.remove(k);
            if let Some(c) = self.current {
                if c > k {
                    self.current = Some(c - 1);
                }
            }
        }
    }

    pub fn game(&self) -> Option<Game> {
        self.game
    }

    /// Takes the notices produced since the last call.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// Marks the next sample as following a session change (used when frames
    /// were routed elsewhere in between, e.g. another profile was active).
    pub fn mark_session_change(&mut self) {
        self.bracket.session_change = true;
    }

    pub fn push(&mut self, f: &Frame) {
        let sess = match self.sess_ids.iter().position(|s| *s == f.sess) {
            Some(i) => i as u32,
            None => {
                self.sess_ids.push(f.sess.clone());
                (self.sess_ids.len() - 1) as u32
            }
        };
        if self.sess != Some(sess) {
            self.bracket.session_change = true;
            self.truckersmp_module = false;
            self.truckersmp_sdk = None;
            self.truckersmp = false;
            self.sess = Some(sess);
        } else if f.seq > 0 && f.seq != self.last_seq + 1 && f.seq > self.last_seq {
            // Frames missing inside one session: a data gap.
            self.bracket.session_change = true;
        }
        if f.seq > 0 {
            self.last_seq = f.seq;
        }
        match &f.body {
            Body::Init { game, .. } => {
                if game.is_some() {
                    self.game = *game;
                }
                if f.wall_ms > 0 {
                    self.evict_finished(f.wall_ms);
                }
                self.bracket.session_change = true;
            }
            Body::TimerRestart => self.bracket.timer_restart = true,
            Body::Config { id, attrs } if JobType::from_config_id(id).is_some() => {
                let Some(game) = self.game else { return };
                let job_type = JobType::from_config_id(id).expect("matched");
                let info = JobInfo::from_config_of(game, job_type, attrs);
                match job_type {
                    JobType::Freight => self.freight_cfg = info,
                    JobType::Car => self.car_cfg = info,
                }
                self.job_cfg = self.freight_cfg.clone().or_else(|| self.car_cfg.clone());
                if self.job_cfg.is_none() {
                    self.bracket.job_emptied = true;
                } else if !attrs.is_empty() && self.bracket.no_job_state {
                    self.bracket.job_after_no_job = true;
                }
            }
            Body::Config { id, attrs } if id == "truck" => {
                if attrs.is_empty() {
                    self.bracket.truck_emptied = true;
                    // Another load starts: whatever it brings is not known yet.
                    self.bracket.no_job_state = false;
                    self.bracket.job_after_no_job = false;
                } else {
                    if self.job_cfg.is_none() {
                        self.bracket.no_job_state = true;
                    }
                    let tid = attr(attrs, "id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let idx = match self.truck_ids.iter().position(|t| *t == tid) {
                        Some(i) => i,
                        None => {
                            self.truck_ids.push(tid);
                            self.truck_ids.len() - 1
                        }
                    } as u32;
                    if !self.truck_ids.is_empty() && idx != self.truck && self.last.is_some() {
                        self.bracket.truck_changed = true;
                    }
                    self.truck = idx;
                }
            }
            Body::Gameplay { id, attrs } => match id.as_str() {
                // Applied at once: the job config may already be empty, and the game can
                // stop before another sample arrives (captured deliveries). Both belong
                // to the job that was current (PLAN §5).
                "job.delivered" => self.deliver(Delivered::from_attrs(attrs), f.wall_ms),
                "car_job.delivered" => {
                    if self.current.is_none() {
                        self.resume_lost_road_trip(f.wall_ms);
                    }
                    self.deliver(Delivered::from_attrs(attrs), f.wall_ms)
                }
                "job.cancelled" | "car_job.cancelled" => {
                    self.cancel(attr(attrs, "cancel.penalty").and_then(|v| v.as_i64()), f.wall_ms)
                }
                "player.use.ferry" | "player.use.train" => {
                    self.bracket.transfer = true;
                    self.bracket.transfer_kind = id.strip_prefix("player.use.").map(str::to_string);
                }
                _ => {}
            },
            Body::Modules { truckersmp } => {
                self.truckersmp_module |= *truckersmp;
                self.truckersmp = self.truckersmp_sdk.unwrap_or(self.truckersmp_module);
            }
            Body::Truckersmp { connected, .. } => {
                self.truckersmp_sdk = Some(*connected);
                self.truckersmp = *connected;
            }
            Body::Sample(s) => self.on_sample(sess, f, s),
            _ => {}
        }
    }

    fn make_point(&self, sess: u32, f: &Frame, s: &Sample) -> Point {
        Point {
            sess,
            seq: f.seq,
            wall_ms: f.wall_ms,
            sim_us: f.sim_us.unwrap_or(0),
            run: self.next_run,
            speed_mps: s.speed_mps.unwrap_or(0.0),
            odometer_km: s.odometer_km,
            fuel_l: s.fuel_l,
            pos: s.pos,
            game_time_min: s.game_time_min,
            local_scale: s.local_scale.filter(|v| *v > 0.0).unwrap_or(1.0),
            cargo_loaded: self.job_cfg.as_ref().is_some_and(|j| j.cargo_loaded),
            trailer_connected: s.trailer_connected.unwrap_or(true),
            truck: self.truck,
            mp_time_offset: s.mp_time_offset.unwrap_or(0),
            truckersmp: self.truckersmp,
            after_teleport: false,
            cargo_damage: s.cargo_damage,
        }
    }

    fn new_run(&mut self) -> u32 {
        self.next_run += 1;
        self.next_run
    }

    fn on_sample(&mut self, sess: u32, f: &Frame, s: &Sample) {
        let bracket = std::mem::take(&mut self.bracket);
        let mut p = self.make_point(sess, f, s);
        let prev = self.last.clone();
        let wall = f.wall_ms;
        let teleport_min = self.cfg.timeline.teleport_min_distance_world_m;

        // Classify the bracket.
        // W only within a running session (at init every config starts out empty).
        // A profile switch or main-menu load keeps the session: W plus timer_restart.
        let world_reload = bracket.job_emptied && bracket.truck_emptied && !bracket.session_change;
        let session = bracket.session_change || bracket.timer_restart;
        // Position jump beyond what driving could cover in the elapsed simulation
        // time (samples can go missing while the truck keeps driving).
        let jump_m = match (&prev, p.pos) {
            (Some(a), Some(b)) => a.pos.map(|a| dist3(a, b)).unwrap_or(0.0),
            _ => 0.0,
        };
        let drivable_m = prev.as_ref().map_or(0.0, |a| {
            let dt = if a.sess == p.sess { p.sim_us.saturating_sub(a.sim_us) as f64 / 1e6 } else { 0.0 };
            dt * MAX_PLAUSIBLE_WORLD_MPS
        });
        let jump_m = if jump_m > drivable_m { jump_m } else { 0.0 };
        let disc = prev.as_ref().is_some_and(|a| self.discontinuity(a, &p, jump_m));
        let is_load = world_reload || (session && disc);

        // A stopped (already-delivered) job ignores everything until the job changes.
        if let Some(i) = self.current {
            if self.jobs[i].state == JobState::Stopped
                && self.job_cfg.as_ref().map(|j| &j.fingerprint) != Some(&self.jobs[i].info.fingerprint)
            {
                self.current = None;
            }
        }

        if is_load {
            p.run = self.new_run();
            self.on_load(&mut p, session, bracket.job_after_no_job, wall);
        } else {
            let teleport = jump_m >= teleport_min;
            if session || bracket.truck_changed {
                p.run = self.new_run();
            }
            p.after_teleport = teleport;
            self.on_continuous(&p, prev.as_ref(), &bracket, session, teleport, jump_m, wall);
        }

        // Record the point on the current job.
        if let Some(i) = self.current {
            if self.jobs[i].state == JobState::Tracking {
                p.cargo_loaded = self.jobs[i].info.cargo_loaded;
                self.jobs[i].kept.push(p.clone());
                self.watch_overspeed(i, &p);
            }
        }
        self.last = Some(p);
    }

    /// Would `b` be impossible to reach from `a` by driving?
    fn discontinuity(&self, a: &Point, b: &Point, jump_m: f64) -> bool {
        let t = &self.cfg.timeline;
        let eps = self.cfg.rules.distance.odometer_epsilon_km;
        if jump_m >= t.teleport_min_distance_world_m {
            return true;
        }
        if a.truck != b.truck {
            return jump_m > t.match_position_tolerance_world_m;
        }
        if let (Some(x), Some(y)) = (a.odometer_km, b.odometer_km) {
            let dt = b.sim_us.saturating_sub(a.sim_us) as f64 / 1e6;
            let max_km = dt * MAX_PLAUSIBLE_WORLD_MPS * b.local_scale.max(a.local_scale) / 1000.0;
            if y < x - eps || y > x + max_km + eps {
                return true;
            }
        }
        if let (Some(x), Some(y)) = (a.fuel_l, b.fuel_l) {
            if (y - x).abs() > t.match_fuel_tolerance_l && jump_m > t.match_position_tolerance_world_m {
                return true;
            }
        }
        false
    }

    fn find_job(&self, fp: &Fingerprint) -> Option<usize> {
        // Route key identifies the job; a different cargo key is a cargo switch.
        self.jobs.iter().rposition(|j| j.info.fingerprint.route == fp.route)
    }

    fn start_job(&mut self, info: JobInfo, start: StartKind, wall: i64) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            game: info.fingerprint.route.game,
            profile: self.profile.clone(),
            first_info: info.clone(),
            info,
            state: JobState::Tracking,
            start,
            kept: Vec::new(),
            reverted: Vec::new(),
            gaps: Vec::new(),
            events: vec![JobEvent::Started { start, wall_ms: wall }],
            cargo_switched: false,
            delivered: None,
            delivered_wall_ms: None,
            reverted_deliveries: Vec::new(),
        });
        self.watch.push(OverspeedWatch::default());
        self.notices.push(Notice::JobStarted { job: id });
        self.jobs.len() - 1
    }

    fn suspend_current(&mut self, wall: i64) {
        if let Some(i) = self.current.take() {
            if self.jobs[i].state == JobState::Tracking {
                self.jobs[i].state = JobState::Suspended;
                self.jobs[i].events.push(JobEvent::Suspended { wall_ms: wall });
            }
        }
    }

    fn cancel(&mut self, penalty: Option<i64>, wall: i64) {
        if let Some(i) = self.current.take() {
            if self.jobs[i].state == JobState::Tracking {
                self.jobs[i].state = JobState::Cancelled;
                self.jobs[i].events.push(JobEvent::Cancelled { penalty, wall_ms: wall });
                self.notices.push(Notice::JobCancelled { job: self.jobs[i].id });
            }
        }
    }

    fn deliver(&mut self, d: Delivered, wall: i64) {
        let Some(i) = self.current else { return };
        let job = &mut self.jobs[i];
        match job.state {
            JobState::Tracking => {
                job.state = JobState::Delivered;
                job.delivered = Some(d);
                job.delivered_wall_ms = Some(wall);
                job.events.push(JobEvent::Delivered { wall_ms: wall });
                self.notices.push(Notice::JobDelivered { job: job.id });
            }
            JobState::Stopped => job.events.push(JobEvent::DuplicateDelivery { wall_ms: wall }),
            _ => {}
        }
        self.current = None;
    }

    /// The game can drop a road trip's config at a load and never send it
    /// again, then still deliver the trip (the owner's ATS road trip,
    /// 2026-10-16: three loads, the config back after the first only). The
    /// delivery belongs to the road trip suspended at that load, if nothing
    /// has replaced it: it resumes, and the stretch it was not tracked becomes
    /// a gap from its last point, so the job goes to review instead of being lost.
    fn resume_lost_road_trip(&mut self, wall: i64) {
        let Some(k) = self.jobs.len().checked_sub(1) else { return };
        let job = &self.jobs[k];
        if job.state != JobState::Suspended || job.info.fingerprint.route.job_type != JobType::Car || self.job_cfg.is_some() {
            return;
        }
        let Some(mut p) = self.last.clone() else { return };
        p.cargo_loaded = job.info.cargo_loaded;
        let since = job.kept.last().map(|q| q.wall_ms);
        self.jobs[k].state = JobState::Tracking;
        self.jobs[k].events.push(JobEvent::Resumed { wall_ms: wall });
        self.current = Some(k);
        self.rebase(k, &p, false, wall);
        // The stretch always counts as a gap: after loads its distance can be
        // near zero and the simulation clock restarts, so time it by the PC clock.
        let job = &mut self.jobs[k];
        if let (Some(g), Some(since)) = (job.gaps.last_mut(), since) {
            if g.to_seq == p.seq && g.to_sess == p.sess {
                g.duration_s = g.duration_s.max((p.wall_ms - since).max(0) as f64 / 1000.0);
            }
        }
        job.kept.push(p);
    }

    /// Applies a config (cargo loaded, cargo switch) to a job about to track.
    fn apply_config(&mut self, i: usize, info: &JobInfo, wall: i64) {
        let job = &mut self.jobs[i];
        if job.info.fingerprint.cargo != info.fingerprint.cargo {
            job.cargo_switched = true;
            job.events.push(JobEvent::CargoSwitch {
                from: Box::new(job.info.fingerprint.cargo.clone()),
                to: Box::new(info.fingerprint.cargo.clone()),
                wall_ms: wall,
            });
        }
        if info.cargo_loaded && !job.info.cargo_loaded {
            job.events.push(JobEvent::CargoLoaded { wall_ms: wall });
        }
        job.info = info.clone();
    }

    #[allow(clippy::too_many_arguments)]
    fn on_continuous(
        &mut self,
        p: &Point,
        prev: Option<&Point>,
        b: &Bracket,
        session: bool,
        teleport: bool,
        jump_m: f64,
        wall: i64,
    ) {
        let cfg = self.job_cfg.clone();
        if teleport {
            if let Some(i) = self.current {
                self.jobs[i].events.push(JobEvent::Teleport {
                    world_m: jump_m,
                    ferry_or_train: b.transfer,
                    via: b.transfer_kind.clone(),
                    wall_ms: wall,
                });
            }
        }
        if b.truck_changed {
            if let Some(i) = self.current {
                self.jobs[i].events.push(JobEvent::TruckChange { wall_ms: wall });
            }
        }
        match (self.current, cfg) {
            (Some(i), Some(info)) if self.jobs[i].info.fingerprint.route == info.fingerprint.route => {
                if session && self.jobs[i].state == JobState::Tracking {
                    self.jobs[i].events.push(JobEvent::SessionContinue { wall_ms: wall });
                    self.bridge(i, p, wall);
                }
                if self.jobs[i].state == JobState::Tracking {
                    self.apply_config(i, &info, wall);
                }
            }
            (Some(_), Some(info)) => {
                // A different job without a load or an end event.
                self.suspend_current(wall);
                self.begin(info, (session || prev.is_none()) && !b.job_after_no_job, wall);
            }
            (Some(_), None) => self.suspend_current(wall),
            (None, Some(info)) => self.begin(info, (session || prev.is_none()) && !b.job_after_no_job, wall),
            (None, None) => {}
        }
    }

    /// Samples went missing but the truck plainly kept driving: count the distance.
    fn bridge(&mut self, i: usize, p: &Point, wall: i64) {
        let job = &mut self.jobs[i];
        let Some(last) = job.kept.last() else { return };
        if last.truck != p.truck {
            return;
        }
        if let (Some(a), Some(b)) = (last.odometer_km, p.odometer_km) {
            if b - a > 0.0 {
                job.gaps.push(Gap {
                    kind: GapKind::Bridge,
                    from_seq: last.seq,
                    to_sess: p.sess,
                    to_seq: p.seq,
                    game_km: Some(b - a),
                    local_scale: p.local_scale,
                    counted: p.cargo_loaded && p.trailer_connected,
                    duration_s: 0.0,
                    wall_ms: wall,
                });
            }
        }
    }

    /// A job config with no current job and no load: a known job returning, or
    /// a new one (tracked if accepted in continuous data, late start otherwise).
    fn begin(&mut self, info: JobInfo, first_seen_after_session: bool, wall: i64) {
        if let Some(k) = self.find_job(&info.fingerprint) {
            match self.jobs[k].state {
                JobState::Suspended | JobState::Tracking => {
                    self.jobs[k].state = JobState::Tracking;
                    self.jobs[k].events.push(JobEvent::Resumed { wall_ms: wall });
                    self.apply_config(k, &info, wall);
                    self.current = Some(k);
                    return;
                }
                JobState::Delivered | JobState::Stopped | JobState::Cancelled => {}
            }
        }
        let kind = if first_seen_after_session { StartKind::LateStart } else { StartKind::Tracked };
        self.current = Some(self.start_job(info, kind, wall));
    }

    /// `accepted_here`: the job appeared after the loaded state had none, so it
    /// was accepted after the load and its start was seen.
    fn on_load(&mut self, p: &mut Point, session: bool, accepted_here: bool, wall: i64) {
        let before = self.current;
        let Some(info) = self.job_cfg.clone() else {
            self.suspend_current(wall);
            return;
        };
        let Some(k) = self.find_job(&info.fingerprint) else {
            self.suspend_current(wall);
            let kind = if accepted_here { StartKind::Tracked } else { StartKind::LateStart };
            self.current = Some(self.start_job(info, kind, wall));
            return;
        };
        if before != Some(k) {
            self.suspend_current(wall);
        }
        match self.jobs[k].state {
            JobState::Delivered => {
                let invalid = crate::rules::evaluate(&self.jobs[k], &self.cfg, &Default::default()).speed.invalid;
                if invalid {
                    let job = &mut self.jobs[k];
                    if let Some(d) = job.delivered.take() {
                        job.reverted_deliveries.push(d);
                    }
                    job.delivered_wall_ms = None;
                    job.events.push(JobEvent::InvalidDeliveryReopened { wall_ms: wall });
                } else {
                    self.jobs[k].state = JobState::Stopped;
                    self.jobs[k].events.push(JobEvent::StoppedAfterDelivery { wall_ms: wall });
                    self.current = Some(k);
                    return;
                }
            }
            JobState::Stopped => {
                self.current = Some(k);
                return;
            }
            JobState::Cancelled => {
                self.jobs[k].events.push(JobEvent::CancellationReverted { wall_ms: wall })
            }
            JobState::Suspended | JobState::Tracking => {}
        }
        self.jobs[k].state = JobState::Tracking;
        if before != Some(k) {
            self.jobs[k].events.push(JobEvent::Resumed { wall_ms: wall });
        }
        self.apply_config(k, &info, wall);
        p.cargo_loaded = self.jobs[k].info.cargo_loaded;
        self.current = Some(k);
        self.rebase(k, p, session, wall);
    }

    /// Places the loaded point `p` on job `k`'s kept timeline (PLAN §5 rows).
    fn rebase(&mut self, k: usize, p: &Point, session: bool, wall: i64) {
        let _ = session;
        if let Some(idx) = self.match_in(&self.jobs[k].kept, p) {
            let job = &mut self.jobs[k];
            let tail: Vec<Point> = job.kept.drain(idx + 1..).collect();
            let n = tail.len();
            if n > 0 {
                job.reverted.push(Branch { fork_seq: job.kept[idx].seq, points: tail, reverted_at_wall_ms: wall });
            }
            job.events.push(JobEvent::LoadTruncate { to_seq: job.kept[idx].seq, reverted_points: n, wall_ms: wall });
            self.watch[k] = OverspeedWatch { notified: self.watch[k].notified, ..Default::default() };
            return;
        }
        // Previously reverted material: restore it (closes "revert, then restore").
        let mut found = None;
        for (bi, br) in self.jobs[k].reverted.iter().enumerate().rev() {
            if let Some(j) = self.match_in(&br.points, p) {
                if self.jobs[k].kept.iter().any(|q| q.seq == br.fork_seq) {
                    found = Some((bi, j));
                    break;
                }
            }
        }
        if let Some((bi, j)) = found {
            let job = &mut self.jobs[k];
            let br = job.reverted.remove(bi);
            let fork = job.kept.iter().position(|q| q.seq == br.fork_seq).expect("checked above");
            let tail: Vec<Point> = job.kept.drain(fork + 1..).collect();
            if !tail.is_empty() {
                job.reverted.push(Branch { fork_seq: br.fork_seq, points: tail, reverted_at_wall_ms: wall });
            }
            let restored: Vec<Point> = br.points[..=j].to_vec();
            let n = restored.len();
            job.kept.extend(restored);
            if j + 1 < br.points.len() {
                let fork_seq = br.points[j].seq;
                job.reverted.push(Branch { fork_seq, points: br.points[j + 1..].to_vec(), reverted_at_wall_ms: wall });
            }
            job.events.push(JobEvent::LoadRestore { from_fork_seq: br.fork_seq, restored_points: n, wall_ms: wall });
            return;
        }
        // Beyond anything seen: a gap from the last kept point.
        let job = &mut self.jobs[k];
        if let Some(last) = job.kept.last() {
            let game_km = match (last.odometer_km, p.odometer_km) {
                (Some(a), Some(b)) if last.truck == p.truck => Some(b - a),
                _ => None,
            };
            let duration_s = p.sim_us.saturating_sub(last.sim_us) as f64 / 1e6;
            job.gaps.push(Gap {
                kind: GapKind::Load,
                from_seq: last.seq,
                to_sess: p.sess,
                to_seq: p.seq,
                game_km,
                local_scale: p.local_scale,
                counted: p.cargo_loaded && p.trailer_connected,
                duration_s,
                wall_ms: wall,
            });
            job.events.push(JobEvent::LoadGap { game_km, wall_ms: wall });
        }
    }

    /// Index of the point in `pts` that `p` was loaded from, if any.
    fn match_in(&self, pts: &[Point], p: &Point) -> Option<usize> {
        let t = &self.cfg.timeline;
        let p_pos = p.pos?;
        let mut best: Option<(usize, f64)> = None;
        for (i, q) in pts.iter().enumerate() {
            let Some(q_pos) = q.pos else { continue };
            let d = dist3(q_pos, p_pos);
            if d > t.match_position_tolerance_world_m {
                continue;
            }
            if q.truck == p.truck {
                if let (Some(a), Some(b)) = (q.fuel_l, p.fuel_l) {
                    if (a - b).abs() > t.match_fuel_tolerance_l {
                        continue;
                    }
                }
                if let (Some(a), Some(b)) = (q.odometer_km, p.odometer_km) {
                    // Position already pins the place; the odometer tells repeat visits apart.
                    if (a - b).abs() > 1.0 {
                        continue;
                    }
                }
            }
            if !p.truckersmp {
                if let (Some(a), Some(b)) = (q.game_time_min, p.game_time_min) {
                    if a.abs_diff(b) > t.match_game_time_tolerance_min_game {
                        continue;
                    }
                }
            }
            // Closest match; later points win ties (the most recent visit).
            match best {
                Some((_, bd)) if d > bd => {}
                _ => best = Some((i, d)),
            }
        }
        best.map(|(i, _)| i)
    }

    fn watch_overspeed(&mut self, i: usize, p: &Point) {
        let Some(game) = self.game else { return };
        let (penalty_mph, _) = self.cfg.rules.speed.limits_for(game, self.jobs[i].info.fingerprint.route.job_type);
        let window_us = (self.cfg.rules.speed.sustained_window_s * 1e6) as u64;
        let prec = self.cfg.rules.speed.compare_precision_mph;
        let w = &mut self.watch[i];
        if w.run != p.run || p.after_teleport {
            w.run = p.run;
            w.since_us = None;
        }
        if crate::units::exceeds(p.mph(), penalty_mph, prec) {
            let since = *w.since_us.get_or_insert(p.sim_us);
            if !w.notified && p.sim_us.saturating_sub(since) > window_us {
                w.notified = true;
                self.notices.push(Notice::PenaltyActivated { job: self.jobs[i].id, wall_ms: p.wall_ms });
            }
        } else {
            w.since_us = None;
        }
    }
}

pub fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Routes frames to one [`Timeline`] per profile (PLAN §6b: profiles are
/// separate timelines). Frames before any profile notice go to "unknown".
#[derive(Serialize, Deserialize)]
pub struct Profiles {
    cfg: Config,
    active: String,
    timelines: Vec<(String, Timeline)>,
    /// Init and module messages of the current session, replayed to a profile
    /// timeline created mid-session so it knows the game and multiplayer state.
    session: Vec<Frame>,
}

impl Profiles {
    pub fn new(cfg: Config) -> Profiles {
        Profiles { cfg, active: "unknown".into(), timelines: Vec::new(), session: Vec::new() }
    }

    fn timeline(&mut self, key: &str) -> &mut Timeline {
        if let Some(i) = self.timelines.iter().position(|(k, _)| k == key) {
            return &mut self.timelines[i].1;
        }
        let mut t = Timeline::new(self.cfg.clone(), key);
        for f in &self.session {
            t.push(f);
        }
        self.timelines.push((key.to_string(), t));
        &mut self.timelines.last_mut().expect("just pushed").1
    }

    pub fn push(&mut self, f: &Frame) {
        if let Body::Profile { key } = &f.body {
            if *key != self.active {
                self.active = key.clone();
                self.timeline(key).mark_session_change();
            }
            return;
        }
        // Session-level facts (game, multiplayer) are shared by every profile.
        let active = self.active.clone();
        if matches!(f.body, Body::Init { .. }) {
            self.session.clear();
        }
        if matches!(f.body, Body::Init { .. } | Body::Modules { .. } | Body::Truckersmp { .. }) {
            self.session.push(f.clone());
            for (k, t) in self.timelines.iter_mut() {
                if *k != active {
                    t.push(f);
                }
            }
        }
        self.timeline(&active).push(f);
    }

    /// Live jobs of every profile timeline (evicted ones excluded).
    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.timelines.iter().flat_map(|(_, t)| t.jobs())
    }

    /// Jobs evicted from any profile timeline since the last call.
    pub fn take_evicted(&mut self) -> Vec<Job> {
        self.timelines.iter_mut().flat_map(|(_, t)| t.take_evicted()).collect()
    }

    /// Notices of every profile timeline since the last call, each with its
    /// profile (job ids are per profile timeline).
    pub fn take_notices(&mut self) -> Vec<(String, Notice)> {
        let mut out = Vec::new();
        for (k, t) in self.timelines.iter_mut() {
            out.extend(t.take_notices().into_iter().map(|n| (k.clone(), n)));
        }
        out
    }

    pub fn active_profile(&self) -> &str {
        &self.active
    }

    /// The plugin sessions a job's points (kept and reverted) came from, in
    /// first-seen order.
    pub fn sessions_of(&self, job: &Job) -> Vec<String> {
        let Some((_, t)) = self.timelines.iter().find(|(k, _)| *k == job.profile) else { return Vec::new() };
        let mut idx: Vec<u32> = job.kept.iter().chain(job.reverted.iter().flat_map(|b| b.points.iter())).map(|p| p.sess).collect();
        idx.sort_unstable();
        idx.dedup();
        idx.into_iter().filter_map(|i| t.sess_ids.get(i as usize).cloned()).collect()
    }

    /// The session id behind a point's `sess` index in a profile's timeline.
    pub fn session_id(&self, profile: &str, idx: u32) -> Option<&str> {
        let (_, t) = self.timelines.iter().find(|(k, _)| k == profile)?;
        t.sess_ids.get(idx as usize).map(String::as_str)
    }

    pub fn into_jobs(self) -> Vec<Job> {
        let mut jobs: Vec<Job> = self.timelines.into_iter().flat_map(|(_, t)| t.into_jobs()).collect();
        jobs.sort_by_key(|j| j.events.first().map(event_wall).unwrap_or(0));
        jobs
    }
}

fn event_wall(e: &JobEvent) -> i64 {
    match e {
        JobEvent::Started { wall_ms, .. }
        | JobEvent::LoadTruncate { wall_ms, .. }
        | JobEvent::LoadRestore { wall_ms, .. }
        | JobEvent::LoadGap { wall_ms, .. }
        | JobEvent::SessionContinue { wall_ms }
        | JobEvent::Teleport { wall_ms, .. }
        | JobEvent::Suspended { wall_ms }
        | JobEvent::Resumed { wall_ms }
        | JobEvent::CargoLoaded { wall_ms }
        | JobEvent::CargoSwitch { wall_ms, .. }
        | JobEvent::TruckChange { wall_ms }
        | JobEvent::Delivered { wall_ms }
        | JobEvent::Cancelled { wall_ms, .. }
        | JobEvent::CancellationReverted { wall_ms }
        | JobEvent::InvalidDeliveryReopened { wall_ms }
        | JobEvent::StoppedAfterDelivery { wall_ms }
        | JobEvent::DuplicateDelivery { wall_ms } => *wall_ms,
    }
}
