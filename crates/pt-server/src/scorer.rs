//! The per-(player, game) scorer (PLAN §7a). On Cloudflare one Durable Object
//! holds a [`Scorer`], so a player's frames are processed strictly in order,
//! one batch at a time.
//!
//! Since 2026-10-20 the scorer keeps nothing large in memory: the timelines'
//! points are in its own SQLite database ([`SqlPoints`]), and so is its state
//! (the timelines without their points, the processed chunks, the job memos).
//! Both are written after every chunk, between two awaits, so they commit
//! together: an object that restarts continues exactly where the last chunk
//! left it. (Before, the whole state went to R2 as one JSON snapshot, and a
//! job driven for two days made it 208 MB: more than an object's 128 MB.)

use std::collections::BTreeMap;

use pt_core::store::TrackId;
use pt_core::timeline::{Job, JobState, Notice, Profiles};
use pt_core::trace::{parse_frame, Body};
use pt_core::Game;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::env::{Host, LocalSql, RowExt, SqlVal};
use crate::http::{ApiError, ApiResult};
use crate::jobs::{self, Facts, Trigger};
use crate::params;
use crate::points::{self, SqlPoints};
use crate::store::{self, User};
use crate::util;

/// Part of the state key: state of another format is not loaded, so every
/// scorer rebuilds once from the raw data (since 2026-10-23 a rebuild keeps
/// stored deliveries as they are, [`jobs::persist`]). 2 (2026-10-15): job memos know when
/// a job was stored, and finished jobs wait in `unstored`. 3 (2026-10-16): road
/// trips (`car_job`) are tracked, so past ones appear. 4 (2026-10-20): points
/// and state in the scorer's own database.
pub const SNAPSHOT_FORMAT: u32 = 4;
/// At most this many chunks per pass. A catch-up after hours offline (or a
/// capture-mode session: three times the frames, in small live chunks) is
/// worked through in several passes, so one never runs out of time or
/// subrequests; the Durable Object starts the next one ([`Report::more`]).
pub const MAX_CHUNKS_PER_PASS: usize = 400;
/// A job still being driven is stored at most this often while only its
/// points grow; any other change stores it at once.
pub const LIVE_STORE_EVERY_MS: i64 = 60_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JobMemo {
    /// What was last stored: changes when the job changes (state, events, points).
    fingerprint: String,
    delivered_at: Option<i64>,
    /// The fingerprint without the point count, and when it was last stored.
    #[serde(default)]
    shape: String,
    #[serde(default)]
    stored_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
struct Snapshot<D> {
    /// [`config::timeline_key`] the timelines were built with.
    key: String,
    profiles: Profiles<SqlPoints<D>>,
    /// Sessions in processing order, with how many chunks are done.
    sessions: Vec<(String, i64)>,
    jobs: BTreeMap<String, JobMemo>,
    /// Latest TruckersMP (server, account) per session, while connected.
    truckersmp: BTreeMap<String, (u64, u64)>,
    /// Jobs that left the timelines, their points still in the store, not
    /// looked at yet.
    #[serde(default)]
    evicted: Vec<Job<TrackId>>,
    /// Finished jobs that left the timelines but could not be stored yet.
    #[serde(default)]
    unstored: Vec<(Job<TrackId>, Facts)>,
    /// A rebuild still working through the raw data in passes. Until it has
    /// caught up, jobs still in the timelines are only part-way replayed and
    /// are not stored (2026-10-15: a refresh wrote a delivered job back to
    /// `tracking` for a few minutes).
    #[serde(default)]
    rebuilding: bool,
}

impl<D: LocalSql + Clone> Snapshot<D> {
    /// An empty state; the database's points are dropped.
    fn new(key: String, cfg: pt_core::Config, db: &D) -> Snapshot<D> {
        let mut store = SqlPoints::open(db.clone());
        store.clear();
        Snapshot {
            key,
            profiles: Profiles::with_store(cfg, store),
            sessions: Vec::new(),
            jobs: BTreeMap::new(),
            truckersmp: BTreeMap::new(),
            evicted: Vec::new(),
            unstored: Vec::new(),
            rebuilding: false,
        }
    }

    /// Writes the points and the state. Synchronous: with no await in between,
    /// they commit together.
    fn commit(&mut self, db: &D) -> ApiResult<()> {
        self.profiles.store_mut().flush();
        let state = serde_json::to_vec(&*self).map_err(|e| ApiError::internal(&e.to_string()))?;
        db.exec("INSERT OR REPLACE INTO pt_kv (k, v) VALUES ('state', ?1)", &[SqlVal::Blob(state)]).map_err(|e| ApiError::internal(&e.0))?;
        Ok(())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub chunks: usize,
    pub frames: usize,
    pub jobs_stored: usize,
    pub rebuilt: bool,
    /// Chunks are left for another pass ([`MAX_CHUNKS_PER_PASS`]).
    pub more: bool,
}

pub struct Scorer<D> {
    pub user_id: String,
    pub game: Game,
    db: D,
    snap: Option<Snapshot<D>>,
}

/// Where the state lived before 2026-10-20 (R2); deleted once replaced.
fn legacy_snapshot_key(user_id: &str, game: Game) -> String {
    format!("snap/{user_id}/{}.json.gz", game.as_str())
}

/// `counts`: kept points, and reverted points over all branches ([`Job::point_counts`]).
fn fingerprint<P>(j: &Job<P>, counts: (usize, usize)) -> String {
    format!("{}|{}", shape(j, counts), counts.0)
}

/// Everything in the fingerprint but the kept points, which grow with every
/// upload while the job is driven.
fn shape<P>(j: &Job<P>, (_, reverted): (usize, usize)) -> String {
    format!("{:?}|{}|{}|{}|{}|{}", j.state, j.events.len(), reverted, j.gaps.len(), j.delivered.is_some(), j.info.cargo_loaded)
}

fn memo_key<P>(j: &Job<P>) -> String {
    format!("{}/{}", j.profile, j.id)
}

/// What a job is stored with besides itself.
fn job_facts<D: LocalSql>(snap: &Snapshot<D>, j: &Job<TrackId>) -> Facts {
    let point_sess = snap.profiles.point_sessions(j);
    let mut from_points = point_sess.clone();
    from_points.sort_unstable();
    from_points.dedup();
    let sessions: Vec<String> =
        from_points.iter().filter_map(|i| snap.profiles.session_id(&j.profile, *i).map(str::to_string)).collect();
    let mut idx = point_sess;
    idx.extend(j.gaps.iter().map(|g| g.to_sess));
    idx.sort_unstable();
    idx.dedup();
    let sess_ids = idx.into_iter().filter_map(|i| snap.profiles.session_id(&j.profile, i).map(|s| (i, s.to_string()))).collect();
    let truckersmp = sessions.iter().rev().find_map(|s| snap.truckersmp.get(s).copied());
    Facts { delivered_at: snap.jobs.get(&memo_key(j)).and_then(|m| m.delivered_at), sessions, sess_ids, truckersmp }
}

impl<D: LocalSql + Clone> Scorer<D> {
    /// `db`: the scorer's own database (its Durable Object's storage).
    pub fn new(user_id: &str, game: Game, db: D) -> Scorer<D> {
        Scorer { user_id: user_id.to_string(), game, db, snap: None }
    }

    /// Loads the state if not in memory. Returns true when there was none
    /// usable and a new, empty one was started.
    ///
    /// `replayed`: the player has raw data already. A state replaced because
    /// its key changed (a new [`SNAPSHOT_FORMAT`], a timeline config change)
    /// then catches up as a rebuild: stored jobs stay as they were, and nobody
    /// is told again (2026-10-23: every deploy re-sent the same speed-penalty
    /// DMs, and re-scored old jobs with the new rules).
    fn load(&mut self, key: &str, cfg: &pt_core::Config, replayed: bool) -> ApiResult<bool> {
        if self.snap.as_ref().is_some_and(|s| s.key == key) {
            return Ok(false);
        }
        points::ensure_schema(&self.db);
        let rows = self.db.exec("SELECT v FROM pt_kv WHERE k = 'state'", &[]).map_err(|e| ApiError::internal(&e.0))?;
        let had_state = !rows.is_empty() || self.snap.is_some();
        let stored = rows.into_iter().next().and_then(|mut r| serde_json::from_slice::<Snapshot<D>>(&r.remove(0).into_bytes()).ok());
        match stored {
            Some(mut s) if s.key == key => {
                s.profiles.store_mut().attach(self.db.clone());
                self.snap = Some(s);
                Ok(false)
            }
            _ => {
                let mut s = Snapshot::new(key.to_string(), cfg.clone(), &self.db);
                s.rebuilding = had_state && replayed;
                s.commit(&self.db)?;
                self.snap = Some(s);
                Ok(true)
            }
        }
    }

    /// Forgets everything; the next [`Scorer::process`] replays all raw data.
    pub async fn reset<H: Host>(&mut self, h: &H) -> ApiResult<()> {
        self.snap = None;
        points::ensure_schema(&self.db);
        for sql in ["DELETE FROM pt_points", "DELETE FROM pt_kv"] {
            self.db.exec(sql, &[]).map_err(|e| ApiError::internal(&e.0))?;
        }
        h.blob_delete(&legacy_snapshot_key(&self.user_id, self.game)).await?;
        Ok(())
    }

    /// Points held in memory now, and bytes of points on disk (tests, logs).
    pub fn footprint(&self) -> (usize, u64) {
        self.snap.as_ref().map_or((0, 0), |s| (s.profiles.store().points_in_memory(), s.profiles.store().stored_bytes()))
    }

    /// Processes every chunk not processed yet, then stores the jobs that changed.
    pub async fn process<H: Host>(&mut self, h: &H) -> ApiResult<Report> {
        let user = store::user_by_id(h, &self.user_id).await?.ok_or_else(|| ApiError::not_found("user"))?;
        let scoring = config::scoring(h).await?;
        let key = format!("{SNAPSHOT_FORMAT}:{}", config::timeline_key(&scoring.cfg));
        let sessions = h
            .all(
                "SELECT sess, chunk_count FROM plugin_sessions WHERE user_id = ?1 AND game = ?2 ORDER BY first_wall_ms, sess",
                params![&self.user_id, self.game.as_str()],
            )
            .await?;
        let sessions: Vec<(String, i64)> = sessions.iter().map(|r| (r.s("sess"), r.i("chunk_count"))).collect();
        let mut report = Report::default();
        if self.load(&key, &scoring.cfg, !sessions.is_empty())? {
            report.rebuilt = self.snap.as_ref().is_some_and(|s| s.rebuilding);
            // The state before 2026-10-20 is not needed any more.
            h.blob_delete(&legacy_snapshot_key(&self.user_id, self.game)).await?;
        }

        let Scorer { db, snap: snap_slot, user_id, game } = self;
        // Data in order: the processed sessions are a prefix of the list, and only
        // the last of them may still grow. Anything else rebuilds from scratch.
        let in_order = {
            let snap = snap_slot.as_ref().expect("loaded");
            let n = snap.sessions.len();
            snap.sessions.iter().enumerate().all(|(i, (s, done))| {
                sessions.get(i).is_some_and(|(sess, count)| sess == s && (i + 1 == n || count == done))
            })
        };
        if !in_order {
            report.rebuilt = true;
            let mut snap = Snapshot::new(key.clone(), scoring.cfg.clone(), db);
            snap.rebuilding = true;
            snap.commit(db)?;
            *snap_slot = Some(snap);
        }
        let snap = snap_slot.as_mut().expect("loaded");
        let trigger = if snap.rebuilding { Trigger::Rebuild } else { Trigger::Upload };

        let mp = scoring.cfg.multiplayer.clone();
        let mut penalties: Vec<(String, u32, i64)> = Vec::new();
        'sessions: for (i, (sess, count)) in sessions.iter().enumerate() {
            let done = snap.sessions.get(i).map_or(0, |(_, d)| *d);
            if done >= *count {
                continue;
            }
            if snap.sessions.len() <= i {
                snap.sessions.push((sess.clone(), 0));
            }
            if report.chunks >= MAX_CHUNKS_PER_PASS {
                report.more = true;
                break;
            }
            // One query for this pass's chunks of the session.
            let budget = (MAX_CHUNKS_PER_PASS - report.chunks) as i64;
            let rows = h
                .all(
                    "SELECT chunk_no, r2_key, received_at FROM raw_chunks WHERE user_id = ?1 AND sess = ?2 AND chunk_no >= ?3
                     ORDER BY chunk_no LIMIT ?4",
                    params![&*user_id, sess, done, budget],
                )
                .await?;
            for (k, chunk_no) in (done..*count).enumerate() {
                if report.chunks >= MAX_CHUNKS_PER_PASS {
                    report.more = true;
                    break 'sessions;
                }
                let row = rows.get(k).filter(|r| r.i("chunk_no") == chunk_no).ok_or_else(|| ApiError::internal("missing chunk"))?;
                let blob = h.blob_get(&row.s("r2_key")).await?.ok_or_else(|| ApiError::internal("missing chunk data"))?;
                let text = util::gunzip(&blob, usize::MAX).map_err(|e| ApiError::internal(&e.to_string()))?;
                let received_at = row.i("received_at");
                // From here to the commit: no await, so the chunk's points and
                // the state that counts it commit together.
                for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    let Ok(line) = std::str::from_utf8(line) else { continue };
                    let Ok(f) = parse_frame(line, &mp) else { continue };
                    if let Body::Truckersmp { connected: true, server_id, account_id, .. } = f.body {
                        if account_id > 0 {
                            snap.truckersmp.insert(f.sess.clone(), (server_id, account_id));
                        }
                    }
                    snap.profiles.push(&f);
                    report.frames += 1;
                }
                // Deliveries take the server's clock of the chunk they arrived in.
                for j in snap.profiles.jobs() {
                    let m = snap.jobs.entry(memo_key(j)).or_default();
                    match (j.delivered.is_some(), m.delivered_at) {
                        (true, None) => m.delivered_at = Some(received_at),
                        (false, Some(_)) => m.delivered_at = None,
                        _ => {}
                    }
                }
                for (profile, n) in snap.profiles.take_notices() {
                    if let Notice::PenaltyActivated { job, wall_ms } = n {
                        penalties.push((profile, job, wall_ms));
                    }
                }
                let evicted = snap.profiles.take_evicted_live();
                snap.evicted.extend(evicted);
                snap.sessions[i].1 = chunk_no + 1;
                report.chunks += 1;
                snap.commit(db)?;
            }
        }

        // Evicted jobs: unchanged since stored, the server's copy is final;
        // otherwise they wait in `unstored` until stored.
        for j in std::mem::take(&mut snap.evicted) {
            let key = memo_key(&j);
            let stored = snap.jobs.get(&key).is_some_and(|m| m.fingerprint == fingerprint(&j, j.point_counts(snap.profiles.store())));
            snap.jobs.remove(&key);
            if stored {
                snap.profiles.release(&j);
                continue;
            }
            let f = job_facts(snap, &j);
            snap.unstored.push((j, f));
        }
        snap.commit(db)?;
        // Finished jobs exist only here now: one that cannot be stored stays
        // for the next pass.
        while let Some((job, f)) = snap.unstored.first().cloned() {
            match jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, snap.profiles.store(), &f, trigger, None).await {
                Ok(stored) => {
                    report.jobs_stored += stored.is_some() as usize;
                    snap.unstored.remove(0);
                    snap.profiles.release(&job);
                    snap.commit(db)?;
                }
                Err(e) => return Err(e),
            }
        }

        let now = h.now_ms();
        // Behind on the raw data: the jobs still in the timelines are stored
        // once a pass reaches the end of it, but delivered ones at once.
        let finished_state = |j: &Job<TrackId>| matches!(j.state, JobState::Delivered | JobState::Stopped);
        let mut changed: Vec<String> = Vec::new();
        for j in snap.profiles.jobs().filter(|j| !report.more || finished_state(j)) {
            let m = snap.jobs.get(&memo_key(j)).cloned().unwrap_or_default();
            let counts = j.point_counts(snap.profiles.store());
            if m.fingerprint == fingerprint(j, counts) {
                continue;
            }
            // Only the points grew: a job being driven waits a little.
            let growing = m.shape == shape(j, counts) && j.delivered.is_none();
            if growing && now - m.stored_ms < LIVE_STORE_EVERY_MS {
                continue;
            }
            changed.push(memo_key(j));
        }
        // Stored straight from the store, one at a time. The memo moves on only
        // once a job is stored, so a failed store is tried again on the next pass.
        for key in changed {
            let Some(job) = snap.profiles.jobs().find(|j| memo_key(j) == key).cloned() else { continue };
            let counts = job.point_counts(snap.profiles.store());
            let (fp, sh) = (fingerprint(&job, counts), shape(&job, counts));
            let f = job_facts(snap, &job);
            let stored = jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, snap.profiles.store(), &f, trigger, None).await?;
            if stored.is_some() {
                report.jobs_stored += 1;
            }
            let m = snap.jobs.entry(key).or_default();
            m.fingerprint = fp;
            m.shape = sh;
            m.stored_ms = now;
            snap.commit(db)?;
        }
        if !report.more && snap.rebuilding {
            snap.rebuilding = false;
            snap.commit(db)?;
        }
        if trigger == Trigger::Upload {
            for (profile, id, wall_ms) in penalties {
                if let Some(j) = snap.profiles.jobs().find(|j| j.profile == profile && j.id == id).cloned() {
                    let r = snap.profiles.evaluate(&j, &scoring.cfg, &Default::default());
                    let job_id = store::one(
                        h,
                        "SELECT job_id FROM jobs WHERE user_id = ?1 AND game = ?2 AND profile_key = ?3 AND local_id = ?4",
                        params![&*user_id, game.as_str(), &profile, id],
                    )
                    .await?
                    .map(|r| r.s("job_id"));
                    jobs::notify(h, &scoring.cfg, &user, "speed_penalty_activated", job_id.as_deref(), *game, j.info.fingerprint.route.job_type, wall_ms, &j.info.cargo_name, &r)
                        .await?;
                }
            }
        }
        Ok(report)
    }
}

/// Re-scores every stored job of this player from its own points, with the
/// current config and bonuses (ADMIN_API §8). A change to the timeline's own
/// settings needs the raw frames instead: [`Scorer::reset`] first.
pub async fn rescore_job<H: Host>(h: &H, job_id: &str, trigger: Trigger, moderator: Option<jobs::Moderator<'_>>) -> ApiResult<()> {
    let row = store::one(h, "SELECT * FROM jobs WHERE job_id = ?1", params![job_id]).await?.ok_or_else(|| ApiError::not_found("job"))?;
    if row.s("origin") == "manual" {
        return crate::manual::rescore(h, &row, trigger, moderator).await;
    }
    let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("user"))?;
    let stored = jobs::load_stored(h, job_id).await?.ok_or_else(|| ApiError::internal("missing job data"))?;
    let detail = row.json("detail");
    let sessions: Vec<String> = serde_json::from_value(detail["plugin_sessions"].clone()).unwrap_or_default();
    let truckersmp = match (row.opt_i("tmp_server_id"), row.opt_i("tmp_account_id")) {
        (Some(s), Some(a)) => Some((s as u64, a as u64)),
        _ => None,
    };
    let facts = Facts { delivered_at: row.opt_i("delivered_at"), sessions, sess_ids: stored.sess_ids.clone(), truckersmp };
    let scoring = config::scoring(h).await?;
    jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &stored.job, &stored, &facts, trigger, moderator).await?;
    Ok(())
}

/// For callers that only have the user (tests, the queue consumer).
pub async fn user_games<H: Host>(h: &H, user: &User) -> ApiResult<Vec<Game>> {
    Ok(h.all("SELECT DISTINCT game FROM plugin_sessions WHERE user_id = ?1 AND game IS NOT NULL", params![&user.user_id])
        .await?
        .iter()
        .filter_map(|r| Game::from_api(&r.s("game")))
        .collect())
}
