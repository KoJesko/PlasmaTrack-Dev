//! The per-(player, game) scorer (PLAN §7a). On Cloudflare one Durable Object
//! holds a [`Scorer`] in memory, so a player's frames are processed strictly in
//! order, one batch at a time. After an eviction it restores the snapshot
//! from R2 and replays only the chunks uploaded since.

use std::collections::BTreeMap;

use pt_core::store::{PointStore, TrackId};
use pt_core::timeline::{Job, JobState, Notice, Profiles};
use pt_core::trace::{parse_frame, Body};
use pt_core::Game;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::env::{Host, RowExt};
use crate::http::{ApiError, ApiResult};
use crate::jobs::{self, Facts, Trigger};
use crate::params;
use crate::store::{self, User};
use crate::util;

/// Save the snapshot at least this often while a player keeps uploading.
pub const SNAPSHOT_EVERY_MS: i64 = 10 * 60_000;
/// Part of the snapshot key: a snapshot of another format is not loaded, so
/// every scorer rebuilds once from the raw data. 2 (2026-10-15): job memos
/// know when a job was stored, and finished jobs wait in `unstored`. 3
/// (2026-10-16): road trips (`car_job`) are tracked, so past ones appear. 4
/// (2026-10-20): a job's points are tracks in the timelines' point store.
pub const SNAPSHOT_FORMAT: u32 = 4;
/// At most this many chunks per pass. A catch-up after hours offline (or a
/// capture-mode session: three times the frames, in small live chunks) is
/// worked through in several passes, so one never runs out of memory or
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
struct Snapshot {
    /// [`config::timeline_key`] the timelines were built with.
    key: String,
    profiles: Profiles,
    /// Sessions in processing order, with how many chunks are done.
    sessions: Vec<(String, i64)>,
    jobs: BTreeMap<String, JobMemo>,
    /// Latest TruckersMP (server, account) per session, while connected.
    truckersmp: BTreeMap<String, (u64, u64)>,
    /// Finished jobs that left the timelines but could not be stored yet.
    #[serde(default)]
    unstored: Vec<(Job, Facts)>,
    /// A rebuild still working through the raw data in passes. Until it has
    /// caught up, jobs still in the timelines are only part-way replayed and
    /// are not stored (2026-10-15: a refresh wrote a delivered job back to
    /// `tracking` for a few minutes).
    #[serde(default)]
    rebuilding: bool,
}

impl Snapshot {
    fn new(key: String, cfg: pt_core::Config) -> Snapshot {
        Snapshot {
            key,
            profiles: Profiles::new(cfg),
            sessions: Vec::new(),
            jobs: BTreeMap::new(),
            truckersmp: BTreeMap::new(),
            unstored: Vec::new(),
            rebuilding: false,
        }
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

pub struct Scorer {
    pub user_id: String,
    pub game: Game,
    snap: Option<Snapshot>,
    saved_at: i64,
    dirty: bool,
}

fn snapshot_key(user_id: &str, game: Game) -> String {
    format!("snap/{user_id}/{}.json.gz", game.as_str())
}

/// `counts`: kept points, and reverted points over all branches
/// ([`Job::point_counts`] for a live job, [`vec_counts`] for one in memory).
fn fingerprint<P>(j: &Job<P>, counts: (usize, usize)) -> String {
    format!("{}|{}", shape(j, counts), counts.0)
}

/// Everything in the fingerprint but the kept points, which grow with every
/// upload while the job is driven.
fn shape<P>(j: &Job<P>, (_, reverted): (usize, usize)) -> String {
    format!("{:?}|{}|{}|{}|{}|{}", j.state, j.events.len(), reverted, j.gaps.len(), j.delivered.is_some(), j.info.cargo_loaded)
}

fn vec_counts(j: &Job) -> (usize, usize) {
    (j.kept.len(), j.reverted.iter().map(|b| b.points.len()).sum())
}

/// The `sess` index of every kept and reverted point of a job in memory.
fn vec_sessions(j: &Job) -> Vec<u32> {
    j.kept.iter().chain(j.reverted.iter().flat_map(|b| b.points.iter())).map(|p| p.sess).collect()
}

fn memo_key<P>(j: &Job<P>) -> String {
    format!("{}/{}", j.profile, j.id)
}

/// What a job is stored with besides itself. `point_sess`: the `sess` index
/// of its kept and reverted points.
fn job_facts<P>(snap: &Snapshot, j: &Job<P>, point_sess: Vec<u32>) -> Facts {
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

impl Scorer {
    pub fn new(user_id: &str, game: Game) -> Scorer {
        Scorer { user_id: user_id.to_string(), game, snap: None, saved_at: 0, dirty: false }
    }

    pub fn needs_save(&self) -> bool {
        self.dirty
    }

    /// Writes the snapshot to R2 if anything changed since the last save.
    pub async fn save<H: Host>(&mut self, h: &H) -> ApiResult<()> {
        let (Some(snap), true) = (&self.snap, self.dirty) else { return Ok(()) };
        let z = util::gzip_json(snap).map_err(|e| ApiError::internal(&e))?;
        h.blob_put(&snapshot_key(&self.user_id, self.game), z).await?;
        self.saved_at = h.now_ms();
        self.dirty = false;
        Ok(())
    }

    async fn load<H: Host>(&mut self, h: &H, key: &str, cfg: &pt_core::Config) -> ApiResult<()> {
        if self.snap.as_ref().is_some_and(|s| s.key == key) {
            return Ok(());
        }
        let stored = match h.blob_get(&snapshot_key(&self.user_id, self.game)).await? {
            Some(z) => util::gunzip_json::<Snapshot>(&z).ok(),
            None => None,
        };
        self.snap = Some(match stored {
            Some(s) if s.key == key => s,
            _ => {
                self.dirty = true;
                Snapshot::new(key.to_string(), cfg.clone())
            }
        });
        Ok(())
    }

    /// Forgets everything; the next [`Scorer::process`] replays all raw data.
    pub async fn reset<H: Host>(&mut self, h: &H) -> ApiResult<()> {
        self.snap = None;
        h.blob_delete(&snapshot_key(&self.user_id, self.game)).await?;
        Ok(())
    }

    /// Processes every chunk not processed yet, then stores the jobs that changed.
    pub async fn process<H: Host>(&mut self, h: &H) -> ApiResult<Report> {
        let user = store::user_by_id(h, &self.user_id).await?.ok_or_else(|| ApiError::not_found("user"))?;
        let scoring = config::scoring(h).await?;
        let key = format!("{SNAPSHOT_FORMAT}:{}", config::timeline_key(&scoring.cfg));
        self.load(h, &key, &scoring.cfg).await?;
        let sessions = h
            .all(
                "SELECT sess, chunk_count FROM plugin_sessions WHERE user_id = ?1 AND game = ?2 ORDER BY first_wall_ms, sess",
                params![&self.user_id, self.game.as_str()],
            )
            .await?;
        let sessions: Vec<(String, i64)> = sessions.iter().map(|r| (r.s("sess"), r.i("chunk_count"))).collect();

        // Data in order: the processed sessions are a prefix of the list, and only
        // the last of them may still grow. Anything else rebuilds from scratch.
        let mut report = Report::default();
        let in_order = {
            let snap = self.snap.as_ref().expect("loaded");
            let n = snap.sessions.len();
            snap.sessions.iter().enumerate().all(|(i, (s, done))| {
                sessions.get(i).is_some_and(|(sess, count)| sess == s && (i + 1 == n || count == done))
            })
        };
        if !in_order {
            report.rebuilt = true;
            let mut snap = Snapshot::new(key.clone(), scoring.cfg.clone());
            snap.rebuilding = true;
            self.snap = Some(snap);
        }
        let trigger = if self.snap.as_ref().expect("loaded").rebuilding { Trigger::Rebuild } else { Trigger::Upload };

        let names = scoring.cfg.multiplayer.truckersmp_module_names.clone();
        let mut evicted: Vec<Job> = Vec::new();
        let mut penalties: Vec<(String, u32, i64)> = Vec::new();
        'sessions: for (i, (sess, count)) in sessions.iter().enumerate() {
            let snap = self.snap.as_mut().expect("loaded");
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
                    params![&self.user_id, sess, done, budget],
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
                let snap = self.snap.as_mut().expect("loaded");
                for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    let Ok(line) = std::str::from_utf8(line) else { continue };
                    let Ok(f) = parse_frame(line, &names) else { continue };
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
                evicted.extend(snap.profiles.take_evicted());
                snap.sessions[i].1 = chunk_no + 1;
                report.chunks += 1;
                self.dirty = true;
            }
        }

        // Store what changed: evicted jobs (final), then the live ones.
        let snap = self.snap.as_mut().expect("loaded");
        let mut finished: Vec<(Job, Facts)> = std::mem::take(&mut snap.unstored);
        let mut changed: Vec<String> = Vec::new();
        let facts = |snap: &Snapshot, j: &Job| job_facts(snap, j, vec_sessions(j));
        for j in evicted {
            let key = memo_key(&j);
            let stored = snap.jobs.get(&key).is_some_and(|m| m.fingerprint == fingerprint(&j, vec_counts(&j)));
            snap.jobs.remove(&key);
            // Offloaded and unchanged since it was stored: the server's copy is final.
            if j.offloaded && stored {
                continue;
            }
            let f = facts(snap, &j);
            finished.push((j, f));
        }
        // Finished jobs exist only here now: one that cannot be stored is kept
        // (in the snapshot) for the next pass.
        let mut finished = finished.into_iter();
        while let Some((job, f)) = finished.next() {
            let (job, f) = if job.offloaded {
                match Self::hydrate(h, &self.user_id, self.game, &job).await {
                    Ok(full) => {
                        let f = facts(snap, &full);
                        (full, f)
                    }
                    Err(e) => {
                        snap.unstored.push((job, f));
                        snap.unstored.extend(finished);
                        self.dirty = true;
                        return Err(e);
                    }
                }
            } else {
                (job, f)
            };
            match jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, &f, trigger, None).await {
                Ok(stored) => report.jobs_stored += stored.is_some() as usize,
                Err(e) => {
                    snap.unstored.push((job, f));
                    snap.unstored.extend(finished);
                    self.dirty = true;
                    return Err(e);
                }
            }
        }
        let now = h.now_ms();
        // Behind on the raw data: the jobs still in the timelines are stored
        // once a pass reaches the end of it, but delivered ones at once, so a
        // rebuild does not hold them all in memory.
        let finished_state = |j: &Job<TrackId>| matches!(j.state, JobState::Delivered | JobState::Stopped);
        let points = snap.profiles.store();
        for j in snap.profiles.jobs().filter(|j| !report.more || finished_state(j)) {
            let m = snap.jobs.get(&memo_key(j)).cloned().unwrap_or_default();
            let counts = j.point_counts(points);
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
        // Stored straight from the timelines, one at a time: a copy of every
        // job at the end of a rebuild took a Durable Object past its 128 MB
        // (the owner's ETS2 rebuild, 2026-10-16). The memo moves on only once
        // a job is stored, so a failed store is tried again on the next pass.
        for key in changed {
            let Some(live) = snap.profiles.jobs().find(|j| memo_key(j) == key) else { continue };
            let counts = live.point_counts(snap.profiles.store());
            let (fp, sh) = (fingerprint(live, counts), shape(live, counts));
            let job = snap.profiles.materialize(live);
            let stored = if job.offloaded {
                let full = Self::hydrate(h, &self.user_id, self.game, &job).await?;
                let f = facts(snap, &full);
                jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &full, &f, trigger, None).await?
            } else {
                let f = facts(snap, &job);
                jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, &f, trigger, None).await?
            };
            if stored.is_some() {
                report.jobs_stored += 1;
            }
            let m = snap.jobs.entry(key).or_default();
            m.fingerprint = fp;
            m.shape = sh;
            m.stored_ms = now;
            self.dirty = true;
        }
        // Finished jobs leave memory once stored (owner, 2026-10-16): only
        // their last point stays, for a gap if a load brings one back.
        let points = snap.profiles.store();
        let done: Vec<(String, u32, bool)> = snap
            .profiles
            .jobs()
            .filter(|j| matches!(j.state, JobState::Delivered | JobState::Stopped | JobState::Cancelled))
            .filter(|j| !j.offloaded || points.len(j.kept) > 1 || !j.reverted.is_empty())
            .filter(|j| snap.jobs.get(&memo_key(j)).is_some_and(|m| m.fingerprint == fingerprint(j, j.point_counts(points))))
            .map(|j| {
                let invalid = j.state != JobState::Cancelled
                    && (j.offloaded_invalid || snap.profiles.evaluate(j, &scoring.cfg, &Default::default()).speed.invalid);
                (j.profile.clone(), j.id, invalid)
            })
            .collect();
        for (profile, id, invalid) in done {
            snap.profiles.offload(&profile, id, invalid);
            let Some(j) = snap.profiles.jobs().find(|j| j.profile == profile && j.id == id) else { continue };
            let counts = j.point_counts(snap.profiles.store());
            let (key, fp, sh) = (memo_key(j), fingerprint(j, counts), shape(j, counts));
            let m = snap.jobs.entry(key).or_default();
            m.fingerprint = fp;
            m.shape = sh;
            self.dirty = true;
        }
        if !report.more && snap.rebuilding {
            snap.rebuilding = false;
            self.dirty = true;
        }
        if trigger == Trigger::Upload {
            for (profile, id, wall_ms) in penalties {
                if let Some(j) = snap.profiles.jobs().find(|j| j.profile == profile && j.id == id) {
                    let r = snap.profiles.evaluate(j, &scoring.cfg, &Default::default());
                    let job_id = store::one(
                        h,
                        "SELECT job_id FROM jobs WHERE user_id = ?1 AND game = ?2 AND profile_key = ?3 AND local_id = ?4",
                        params![&self.user_id, self.game.as_str(), &profile, id],
                    )
                    .await?
                    .map(|r| r.s("job_id"));
                    jobs::notify(h, &scoring.cfg, &user, "speed_penalty_activated", job_id.as_deref(), self.game, j.info.fingerprint.route.job_type, wall_ms, &j.info.cargo_name, &r)
                        .await?;
                }
            }
        }
        if report.chunks > 0 {
            self.snap.as_mut().expect("loaded").profiles.shrink_to_fit();
        }
        if self.dirty && h.now_ms() - self.saved_at >= SNAPSHOT_EVERY_MS {
            self.save(h).await?;
        }
        Ok(report)
    }

    /// A job whose earlier points are on disk only (`Job::offloaded`): the stored
/// points and the ones since, together, for storing it again.
async fn hydrate<H: Host>(h: &H, user_id: &str, game: Game, job: &Job) -> ApiResult<Job> {
    let mut full = job.clone();
    full.offloaded = false;
    let row = store::one(
        h,
        "SELECT job_id FROM jobs WHERE user_id = ?1 AND game = ?2 AND profile_key = ?3 AND local_id = ?4",
        params![user_id, game.as_str(), &job.profile, job.id],
    )
    .await?;
    let Some(row) = row else { return Ok(full) };
    let Some(z) = h.blob_get(&jobs::blob_key(&row.s("job_id"))).await? else { return Ok(full) };
    let (stored, _) = jobs::decode_blob(&z)?;
    // `kept[0]` is the last point the stored copy has.
    let at = job.kept.first().and_then(|p| stored.kept.iter().position(|q| q.sess == p.sess && q.seq == p.seq));
    let mut kept = stored.kept;
    match at {
        Some(i) => {
            kept.truncate(i + 1);
            kept.extend(job.kept.iter().skip(1).cloned());
        }
        None => kept.extend(job.kept.iter().cloned()),
    }
    full.kept = kept;
    full.reverted = stored.reverted.into_iter().chain(job.reverted.iter().cloned()).collect();
    Ok(full)
}

/// Re-scores every stored job of this player from its own points, with the
    /// current config and bonuses (ADMIN_API §8). A change to the timeline's
    /// own settings needs the raw frames instead: [`Scorer::reset`] first.
    pub async fn rescore_job<H: Host>(h: &H, job_id: &str, trigger: Trigger, moderator: Option<jobs::Moderator<'_>>) -> ApiResult<()> {
        let row = store::one(h, "SELECT * FROM jobs WHERE job_id = ?1", params![job_id]).await?.ok_or_else(|| ApiError::not_found("job"))?;
        if row.s("origin") == "manual" {
            return crate::manual::rescore(h, &row, trigger, moderator).await;
        }
        let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("user"))?;
        let blob = h.blob_get(&jobs::blob_key(job_id)).await?.ok_or_else(|| ApiError::internal("missing job data"))?;
        let (job, sess_ids) = jobs::decode_blob(&blob)?;
        let detail = row.json("detail");
        let sessions: Vec<String> = serde_json::from_value(detail["plugin_sessions"].clone()).unwrap_or_default();
        let truckersmp = match (row.opt_i("tmp_server_id"), row.opt_i("tmp_account_id")) {
            (Some(s), Some(a)) => Some((s as u64, a as u64)),
            _ => None,
        };
        let facts = Facts { delivered_at: row.opt_i("delivered_at"), sessions, sess_ids, truckersmp };
        let scoring = config::scoring(h).await?;
        jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, &facts, trigger, moderator).await?;
        Ok(())
    }
}

/// For callers that only have the user (tests, the queue consumer).
pub async fn user_games<H: Host>(h: &H, user: &User) -> ApiResult<Vec<Game>> {
    Ok(h.all("SELECT DISTINCT game FROM plugin_sessions WHERE user_id = ?1 AND game IS NOT NULL", params![&user.user_id])
        .await?
        .iter()
        .filter_map(|r| Game::from_api(&r.s("game")))
        .collect())
}
