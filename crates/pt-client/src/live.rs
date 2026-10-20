//! Provisional results while you drive (PLAN §6b). The client runs the same
//! `pt-core` timeline and rules as the server on the frames it stores, one
//! [`Profiles`] per game, and raises the two player alerts: an Invalid delivery
//! and the moment the speed penalty activates. The server's result is
//! authoritative; nothing computed here is uploaded.

use std::collections::{HashMap, VecDeque};

use pt_core::rules::{evaluate, Context, JobResult, Outcome};
use pt_core::timeline::{Job, JobState, Notice, Profiles, StartKind};
use pt_core::trace::{parse_frame, Body};
use pt_core::{Config, Game};
use serde::Serialize;

/// Finished jobs kept for the status page after they left the live timeline.
const FINISHED_KEPT: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Alert {
    InvalidDelivery { game: Game, cargo: String, peak_mph: f64 },
    PenaltyActivated { game: Game, cargo: String },
}

impl Alert {
    /// Title and text of the desktop notification.
    pub fn message(&self, cfg: &Config) -> (String, String) {
        match self {
            Alert::InvalidDelivery { game, cargo, peak_mph } => (
                "Invalid delivery".into(),
                format!(
                    "{cargo}: you drove over {} mph (peak {peak_mph:.0} mph), so this delivery scores no points. \
                     Moderators can still approve it.",
                    cfg.rules.speed.limits(*game).invalid_over_mph
                ),
            ),
            Alert::PenaltyActivated { game, cargo } => (
                "Speed penalty".into(),
                format!(
                    "{cargo}: you drove over {} mph for too long. This job's distance now counts at {}×.",
                    cfg.rules.speed.limits(*game).penalty_over_mph,
                    cfg.rules.speed_penalty_factor
                ),
            ),
        }
    }
}

/// One job as the status page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobView {
    pub game: Game,
    pub profile: String,
    pub cargo: String,
    pub source_city: String,
    pub destination_city: String,
    pub state: JobState,
    pub start: StartKind,
    pub outcome: Outcome,
    pub points: f64,
    pub world_km: f64,
    pub game_km: f64,
    pub peak_mph: f64,
    pub reasons: Vec<String>,
    pub started_ms: i64,
    pub updated_ms: i64,
}

impl JobView {
    fn new<P>(job: &Job<P>, r: &JobResult) -> JobView {
        let route = &job.info.fingerprint.route;
        let wall = |e: Option<&pt_core::timeline::JobEvent>| e.map(event_wall).unwrap_or(0);
        let mut reasons: Vec<String> = r.review.iter().map(|x| x.code().to_string()).collect();
        if r.speed.invalid {
            reasons.push("overspeed_invalid".into());
        } else if r.speed.penalty {
            reasons.push("speed_penalty".into());
        }
        JobView {
            game: job.game,
            profile: job.profile.clone(),
            cargo: job.info.cargo_name.clone(),
            source_city: route.source_city.clone(),
            destination_city: route.destination_city.clone(),
            state: job.state,
            start: job.start,
            outcome: r.outcome,
            points: r.points,
            world_km: r.distances.world_km,
            game_km: r.distances.counted_game_km,
            peak_mph: r.speed.peak_mph,
            reasons,
            started_ms: wall(job.events.first()),
            updated_ms: wall(job.events.last()),
        }
    }
}

fn event_wall(e: &pt_core::timeline::JobEvent) -> i64 {
    serde_json::to_value(e).ok().and_then(|v| v["wall_ms"].as_i64()).unwrap_or(0)
}

pub struct LiveJobs {
    cfg: Config,
    games: Vec<(Game, Profiles)>,
    sess_game: HashMap<String, Game>,
    finished: VecDeque<JobView>,
}

impl LiveJobs {
    pub fn new(cfg: Config) -> LiveJobs {
        LiveJobs { cfg, games: Vec::new(), sess_game: HashMap::new(), finished: VecDeque::new() }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Tells which game a session belongs to before its `init` is seen again
    /// (client restarted mid-session).
    pub fn set_session_game(&mut self, sess: &str, game: Game) {
        self.sess_game.insert(sess.to_string(), game);
    }

    fn game_index(&mut self, game: Game) -> usize {
        if let Some(i) = self.games.iter().position(|(g, _)| *g == game) {
            return i;
        }
        self.games.push((game, Profiles::new(self.cfg.clone())));
        self.games.len() - 1
    }

    /// Pushes one stored frame or client note, in stream order. Returns the
    /// alerts it raised.
    pub fn push(&mut self, raw: &str) -> Vec<Alert> {
        let Ok(f) = parse_frame(raw, &self.cfg.multiplayer.truckersmp_module_names) else { return Vec::new() };
        if let Body::Init { game: Some(g), .. } = &f.body {
            self.sess_game.insert(f.sess.clone(), *g);
        }
        let Some(&game) = self.sess_game.get(&f.sess) else { return Vec::new() };
        let gi = self.game_index(game);
        self.games[gi].1.push(&f);
        let notices = self.games[gi].1.take_notices();
        let evicted = self.games[gi].1.take_evicted();
        let mut alerts = Vec::new();
        for (profile, n) in notices {
            let (Notice::JobDelivered { job } | Notice::PenaltyActivated { job, .. }) = &n else { continue };
            let profiles = &self.games[gi].1;
            let Some(j) = profiles.jobs().find(|j| j.profile == profile && j.id == *job) else { continue };
            let cargo = j.info.cargo_name.clone();
            match n {
                Notice::JobDelivered { .. } => {
                    let r = profiles.evaluate(j, &self.cfg, &Context::default());
                    if r.speed.invalid {
                        alerts.push(Alert::InvalidDelivery { game, cargo, peak_mph: r.speed.peak_mph });
                    }
                }
                _ => alerts.push(Alert::PenaltyActivated { game, cargo }),
            }
        }
        for j in evicted {
            let r = evaluate(&j, &self.cfg, &Context::default());
            self.finished.push_front(JobView::new(&j, &r));
            self.finished.truncate(FINISHED_KEPT);
        }
        alerts
    }

    /// Every job, newest first: the live timelines, then recently evicted ones.
    pub fn jobs(&self) -> Vec<JobView> {
        let mut out: Vec<JobView> = self
            .games
            .iter()
            .flat_map(|(_, p)| p.jobs().map(move |j| JobView::new(j, &p.evaluate(j, &self.cfg, &Context::default()))))
            .collect();
        out.sort_by_key(|v| std::cmp::Reverse(v.started_ms));
        out.extend(self.finished.iter().cloned());
        out
    }
}

/// A uniformly random delay in `[min, max]` seconds at microsecond resolution,
/// from the OS random source (PLAN decision 10).
pub fn alert_delay(cfg: &Config) -> std::time::Duration {
    let (lo, hi) = cfg.notifications.delay_range_us();
    let mut b = [0u8; 8];
    let r = if getrandom::fill(&mut b).is_ok() { u64::from_le_bytes(b) } else { 0 };
    let span = hi.saturating_sub(lo) + 1;
    std::time::Duration::from_micros(lo + r % span)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::from_toml_str(crate::paths::SAMPLE_CONFIG).unwrap()
    }

    fn line(kind: &str, seq: u64, wall: i64, extra: serde_json::Value) -> String {
        let mut v = serde_json::json!({"v": 1, "kind": kind, "sess": "s1", "seq": seq, "wall_ms": wall, "sim_us": seq * 50_000});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v.to_string()
    }

    #[test]
    fn frames_of_unknown_sessions_are_ignored_until_the_game_is_known() {
        let mut live = LiveJobs::new(cfg());
        assert!(live.push(&line("sample", 5, 1, serde_json::json!({"ch": {}}))).is_empty());
        assert!(live.games.is_empty());
        live.set_session_game("s1", Game::Ats);
        live.push(&line("sample", 6, 2, serde_json::json!({"ch": {}})));
        assert_eq!(live.games.len(), 1);
        assert!(live.jobs().is_empty());
    }

    /// A minimal ATS session: accept with the cargo loaded, drive `secs` at
    /// `mph`, deliver. Shaped like the replay simulator (pt-replay tests/sim).
    fn job_at(mph: f64, secs: f64) -> Vec<String> {
        let mut out = Vec::new();
        let (mut seq, mut sim, mut x, mut odo) = (0u64, 0u64, 0.0f64, 1000.0f64);
        let mut emit = |kind: &str, extra: serde_json::Value, sim: u64| {
            seq += 1;
            let mut v = serde_json::json!({"v": 1, "kind": kind, "sess": "s1", "seq": seq, "wall_ms": 1_790_000_000_000i64 + (sim / 1000) as i64, "sim_us": sim});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            out.push(v.to_string());
        };
        let a = |n: &str, v: serde_json::Value| serde_json::json!({"name": n, "index": null, "value": v});
        let job = serde_json::json!([
            a("cargo.id", "tyres".into()), a("cargo", "Tyres".into()), a("source.city.id", "chicago".into()),
            a("destination.city.id", "joliet".into()), a("source.company.id", "a".into()),
            a("destination.company.id", "b".into()), a("cargo.loaded", true.into()), a("job.market", "cargo_market".into()),
        ]);
        emit("init", serde_json::json!({"game_id": "ats", "game_version": {"major": 1, "minor": 7}}), 0);
        emit("config", serde_json::json!({"id": "truck", "attrs": [a("id", "volvo".into())]}), 0);
        let sample = |x: f64, odo: f64, v: f64| {
            serde_json::json!({"ch": {"truck.speed": v, "truck.odometer": odo, "truck.fuel.amount": 500.0,
                "truck.world.placement": {"position": {"x": x, "y": 0.0, "z": 0.0}}, "game.time": 1000,
                "local.scale": 20.0, "trailer.0.connected": true}})
        };
        emit("sample", sample(x, odo, 0.0), sim);
        emit("config", serde_json::json!({"id": "job", "attrs": job}), sim);
        let v = mph * 0.44704;
        for _ in 0..(secs * 20.0) as u64 {
            sim += 50_000;
            x += v * 0.05;
            odo += v * 0.05 * 20.0 / 1000.0;
            emit("sample", sample(x, odo, v), sim);
        }
        let km = serde_json::json!([a("distance.km", ((odo - 1000.0) as u64).into())]);
        emit("gameplay", serde_json::json!({"id": "job.delivered", "attrs": km}), sim);
        emit("config", serde_json::json!({"id": "job", "attrs": []}), sim);
        sim += 50_000;
        emit("sample", sample(x, odo, 0.0), sim);
        out
    }

    #[test]
    fn a_clean_job_shows_up_without_alerts() {
        let mut live = LiveJobs::new(cfg());
        let alerts: Vec<Alert> = job_at(60.0, 120.0).iter().flat_map(|l| live.push(l)).collect();
        assert!(alerts.is_empty(), "{alerts:?}");
        let jobs = live.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].state, jobs[0].cargo.as_str()), (JobState::Delivered, "Tyres"));
        assert!(jobs[0].world_km > 3.0, "{:?}", jobs[0]);
    }

    #[test]
    fn overspeed_raises_the_penalty_then_the_invalid_delivery() {
        let mut live = LiveJobs::new(cfg());
        let alerts: Vec<Alert> = job_at(160.0, 120.0).iter().flat_map(|l| live.push(l)).collect();
        assert_eq!(alerts.len(), 2, "{alerts:?}");
        assert!(matches!(alerts[0], Alert::PenaltyActivated { game: Game::Ats, .. }));
        assert!(matches!(&alerts[1], Alert::InvalidDelivery { peak_mph, .. } if (*peak_mph - 160.0).abs() < 0.1));
        let (title, text) = alerts[1].message(live.config());
        assert_eq!(title, "Invalid delivery");
        assert!(text.starts_with("Tyres: you drove over 125 mph"), "{text}");
        assert_eq!(live.jobs()[0].outcome, Outcome::Invalid);
    }

    #[test]
    fn delay_stays_inside_the_configured_range() {
        let c = cfg();
        for _ in 0..200 {
            let d = alert_delay(&c).as_secs_f64();
            assert!((c.notifications.delay_min_wall_s..=c.notifications.delay_max_wall_s).contains(&d), "{d}");
        }
    }
}

/// Frames on their way to the live timelines: everything stored at `stored_ms`,
/// in stream order. Cheap to clone; sending never blocks.
#[derive(Clone)]
pub struct Feed(std::sync::mpsc::Sender<(i64, Vec<String>)>);

impl Feed {
    pub fn send(&self, stored_ms: i64, raws: Vec<String>) {
        if !raws.is_empty() {
            let _ = self.0.send((stored_ms, raws));
        }
    }
}

/// Starts the live worker thread. It first rebuilds the timelines from the
/// sessions seen since `since_ms` (no alerts for those), then follows the feed.
/// Frames stored before the rebuild began are in the rebuild, so the feed
/// skips them.
pub fn spawn_worker(
    jobs: std::sync::Arc<std::sync::Mutex<LiveJobs>>,
    db: std::sync::Arc<std::sync::Mutex<crate::db::Db>>,
    since_ms: i64,
    alerts: tokio::sync::mpsc::UnboundedSender<Alert>,
) -> Feed {
    let (tx, rx) = std::sync::mpsc::channel::<(i64, Vec<String>)>();
    std::thread::spawn(move || {
        let t0 = crate::now_ms();
        let sessions = db.lock().expect("db").sessions_since(since_ms).unwrap_or_default();
        let mut frames = 0usize;
        for (sess, game_id) in sessions {
            let Ok(stream) = db.lock().expect("db").session_stream(&sess, t0) else { continue };
            let mut live = jobs.lock().expect("live");
            if let Some(g) = game_id.as_deref().and_then(Game::from_sdk_game_id) {
                live.set_session_game(&sess, g);
            }
            frames += stream.len();
            for raw in &stream {
                live.push(raw);
            }
        }
        crate::log!("live results rebuilt from {frames} stored frames in {} ms", crate::now_ms() - t0);
        while let Ok((stored_ms, raws)) = rx.recv() {
            if stored_ms < t0 {
                continue;
            }
            let mut live = jobs.lock().expect("live");
            for raw in &raws {
                for a in live.push(raw) {
                    let _ = alerts.send(a);
                }
            }
        }
    });
    Feed(tx)
}
