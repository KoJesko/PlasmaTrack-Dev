//! The per-(player, game) scorer (PLAN §7a). On Cloudflare one Durable Object
//! holds a [`Scorer`] in memory, so a player's frames are processed strictly in
//! order, one batch at a time. After an eviction it restores the snapshot
//! from R2 and replays only the chunks uploaded since.

use std::collections::BTreeMap;

use pt_core::timeline::{Job, Notice, Profiles};
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JobMemo {
    /// Changes when the job changes (state, events, points).
    fingerprint: String,
    delivered_at: Option<i64>,
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
}

impl Snapshot {
    fn new(key: String, cfg: pt_core::Config) -> Snapshot {
        Snapshot { key, profiles: Profiles::new(cfg), sessions: Vec::new(), jobs: BTreeMap::new(), truckersmp: BTreeMap::new() }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub chunks: usize,
    pub frames: usize,
    pub jobs_stored: usize,
    pub rebuilt: bool,
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

fn fingerprint(j: &Job) -> String {
    let reverted: usize = j.reverted.iter().map(|b| b.points.len()).sum();
    format!(
        "{:?}|{}|{}|{}|{}|{}|{}",
        j.state,
        j.events.len(),
        j.kept.len(),
        reverted,
        j.gaps.len(),
        j.delivered.is_some(),
        j.info.cargo_loaded
    )
}

fn memo_key(j: &Job) -> String {
    format!("{}/{}", j.profile, j.id)
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
        let json = serde_json::to_vec(snap).map_err(|e| ApiError::internal(&e.to_string()))?;
        h.blob_put(&snapshot_key(&self.user_id, self.game), util::gzip(&json)).await?;
        self.saved_at = h.now_ms();
        self.dirty = false;
        Ok(())
    }

    async fn load<H: Host>(&mut self, h: &H, key: &str, cfg: &pt_core::Config) -> ApiResult<()> {
        if self.snap.as_ref().is_some_and(|s| s.key == key) {
            return Ok(());
        }
        let stored = match h.blob_get(&snapshot_key(&self.user_id, self.game)).await? {
            Some(z) => util::gunzip(&z, usize::MAX).ok().and_then(|j| serde_json::from_slice::<Snapshot>(&j).ok()),
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
        let key = config::timeline_key(&scoring.cfg);
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
        let trigger = if in_order {
            Trigger::Upload
        } else {
            report.rebuilt = true;
            self.snap = Some(Snapshot::new(key.clone(), scoring.cfg.clone()));
            Trigger::Rebuild
        };

        let names = scoring.cfg.multiplayer.truckersmp_module_names.clone();
        let mut evicted: Vec<Job> = Vec::new();
        let mut penalties: Vec<(String, u32, i64)> = Vec::new();
        for (i, (sess, count)) in sessions.iter().enumerate() {
            let snap = self.snap.as_mut().expect("loaded");
            let done = snap.sessions.get(i).map_or(0, |(_, d)| *d);
            if done >= *count {
                continue;
            }
            if snap.sessions.len() <= i {
                snap.sessions.push((sess.clone(), 0));
            }
            for chunk_no in done..*count {
                let row = store::one(
                    h,
                    "SELECT r2_key, received_at FROM raw_chunks WHERE user_id = ?1 AND sess = ?2 AND chunk_no = ?3",
                    params![&self.user_id, sess, chunk_no],
                )
                .await?
                .ok_or_else(|| ApiError::internal("missing chunk"))?;
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
        let mut changed: Vec<(Job, Facts)> = Vec::new();
        let facts = |snap: &Snapshot, j: &Job| -> Facts {
            let sessions = snap.profiles.sessions_of(j);
            let mut idx: Vec<u32> = j.kept.iter().chain(j.reverted.iter().flat_map(|b| b.points.iter())).map(|p| p.sess).collect();
            idx.extend(j.gaps.iter().map(|g| g.to_sess));
            idx.sort_unstable();
            idx.dedup();
            let sess_ids =
                idx.into_iter().filter_map(|i| snap.profiles.session_id(&j.profile, i).map(|s| (i, s.to_string()))).collect();
            let truckersmp = sessions.iter().rev().find_map(|s| snap.truckersmp.get(s).copied());
            Facts { delivered_at: snap.jobs.get(&memo_key(j)).and_then(|m| m.delivered_at), sessions, sess_ids, truckersmp }
        };
        for j in evicted {
            let f = facts(snap, &j);
            snap.jobs.remove(&memo_key(&j));
            changed.push((j, f));
        }
        for j in snap.profiles.jobs() {
            let fp = fingerprint(j);
            let m = snap.jobs.entry(memo_key(j)).or_default();
            if m.fingerprint != fp {
                m.fingerprint = fp;
                let f = facts(snap, j);
                changed.push((j.clone(), f));
            }
        }
        for (job, f) in &changed {
            if jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, job, f, trigger).await?.is_some() {
                report.jobs_stored += 1;
            }
        }
        if trigger == Trigger::Upload {
            for (profile, id, wall_ms) in penalties {
                if let Some(j) = snap.profiles.jobs().find(|j| j.profile == profile && j.id == id) {
                    let r = pt_core::rules::evaluate(j, &scoring.cfg, &Default::default());
                    let job_id = store::one(
                        h,
                        "SELECT job_id FROM jobs WHERE user_id = ?1 AND game = ?2 AND profile_key = ?3 AND local_id = ?4",
                        params![&self.user_id, self.game.as_str(), &profile, id],
                    )
                    .await?
                    .map(|r| r.s("job_id"));
                    jobs::notify(h, &scoring.cfg, &user, "speed_penalty_activated", job_id.as_deref(), self.game, wall_ms, &j.info.cargo_name, &r)
                        .await?;
                }
            }
        }
        if self.dirty && h.now_ms() - self.saved_at >= SNAPSHOT_EVERY_MS {
            self.save(h).await?;
        }
        Ok(report)
    }

    /// Re-scores every stored job of this player from its own points, with the
    /// current config and bonuses (ADMIN_API §8). A change to the timeline's
    /// own settings needs the raw frames instead: [`Scorer::reset`] first.
    pub async fn rescore_job<H: Host>(h: &H, job_id: &str, trigger: Trigger) -> ApiResult<()> {
        let row = store::one(h, "SELECT * FROM jobs WHERE job_id = ?1", params![job_id]).await?.ok_or_else(|| ApiError::not_found("job"))?;
        let user = store::user_by_id(h, &row.s("user_id")).await?.ok_or_else(|| ApiError::not_found("user"))?;
        let blob = h.blob_get(&jobs::blob_key(job_id)).await?.ok_or_else(|| ApiError::internal("missing job data"))?;
        let v: serde_json::Value = serde_json::from_slice(&util::gunzip(&blob, usize::MAX).map_err(|e| ApiError::internal(&e.to_string()))?)
            .map_err(|e| ApiError::internal(&e.to_string()))?;
        let job: Job = serde_json::from_value(v["job"].clone()).map_err(|e| ApiError::internal(&e.to_string()))?;
        let sess_ids: Vec<(u32, String)> = serde_json::from_value(v["sess_ids"].clone()).unwrap_or_default();
        let detail = row.json("detail");
        let sessions: Vec<String> = serde_json::from_value(detail["plugin_sessions"].clone()).unwrap_or_default();
        let truckersmp = match (row.opt_i("tmp_server_id"), row.opt_i("tmp_account_id")) {
            (Some(s), Some(a)) => Some((s as u64, a as u64)),
            _ => None,
        };
        let facts = Facts { delivered_at: row.opt_i("delivered_at"), sessions, sess_ids, truckersmp };
        let scoring = config::scoring(h).await?;
        jobs::persist(h, &scoring.cfg, scoring.version_id.as_deref(), &user, &job, &facts, trigger).await?;
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
