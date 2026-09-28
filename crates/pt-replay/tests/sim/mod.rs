//! A tiny game simulator that writes plugin frames (docs/WIRE.md JSON), shaped
//! after the real captures in PLAN §3: loads empty `job` + `truck` inside a
//! pause, a new session starts with empty configs and `timer_restart`, samples
//! stop while paused, the odometer advances at world distance × `local.scale`.
#![allow(dead_code)]

use pt_core::rules::{evaluate, Context, JobResult};
use pt_core::timeline::Job;
use pt_replay::{parse_lines, replay, sample_config, Replayed};
use serde_json::{json, Value};

pub const MPS_PER_MPH: f64 = 1.0 / 2.23694;

#[derive(Clone, Debug)]
pub struct JobSpec {
    pub cargo: String,
    pub source: String,
    pub destination: String,
    pub planned_km: u64,
    pub market: String,
    pub loaded: bool,
    pub deadline: u64,
}

impl JobSpec {
    pub fn new(cargo: &str) -> JobSpec {
        JobSpec {
            cargo: cargo.into(),
            source: "chicago".into(),
            destination: "sioux_falls".into(),
            planned_km: 1042,
            market: "cargo_market".into(),
            loaded: true,
            deadline: 10_402_602,
        }
    }
    pub fn to(mut self, dest: &str) -> JobSpec {
        self.destination = dest.into();
        self
    }
    pub fn market(mut self, m: &str) -> JobSpec {
        self.market = m.into();
        self
    }
    pub fn empty_leg(mut self) -> JobSpec {
        self.loaded = false;
        self
    }
}

/// Everything a save file restores.
#[derive(Clone, Debug)]
pub struct Snap {
    pub odo: f64,
    pub fuel: f64,
    pub pos: [f64; 3],
    pub gt: f64,
    pub job: Option<JobSpec>,
    pub truck: String,
    pub connected: bool,
}

pub struct Sim {
    pub game: &'static str,
    sess_n: u32,
    pub sess: String,
    seq: u64,
    pub wall_ms: i64,
    pub sim_us: u64,
    pub odo: f64,
    pub fuel: f64,
    pub pos: [f64; 3],
    pub gt: f64,
    pub scale: f64,
    pub truck: String,
    pub connected: bool,
    pub mp_offset: i64,
    pub job: Option<JobSpec>,
    pub lines: Vec<String>,
    paused: bool,
}

impl Sim {
    /// A fresh game session in the world, unpaused, with no job.
    pub fn new(game: &'static str) -> Sim {
        Sim::with_job(game, None)
    }

    /// A session that starts with a job already in the save (a late start).
    pub fn with_job(game: &'static str, job: Option<JobSpec>) -> Sim {
        let mut s = Sim {
            game,
            sess_n: 0,
            sess: String::new(),
            seq: 0,
            wall_ms: 1_790_000_000_000,
            sim_us: 0,
            odo: 12_000.0,
            fuel: 800.0,
            pos: [1000.0, 10.0, -5000.0],
            gt: 10_400_000.0,
            scale: if game == "ats" { 20.0 } else { 19.0 },
            truck: "vehicle.volvo.vnl2025".into(),
            connected: true,
            mp_offset: 0,
            job,
            lines: Vec::new(),
            paused: true,
        };
        s.boot(true);
        s
    }

    fn emit(&mut self, kind: &str, mut body: Value) {
        self.seq += 1;
        self.wall_ms += 5;
        let o = body.as_object_mut().expect("object");
        o.insert("v".into(), json!(1));
        o.insert("kind".into(), json!(kind));
        o.insert("sess".into(), json!(self.sess));
        o.insert("seq".into(), json!(self.seq));
        o.insert("wall_ms".into(), json!(self.wall_ms));
        o.insert("sim_us".into(), json!(self.sim_us));
        self.lines.push(body.to_string());
    }

    fn job_attrs(&self, j: &JobSpec) -> Value {
        let a = |n: &str, t: &str, v: Value| json!({"name": n, "index": null, "type": t, "value": v});
        json!([
            a("cargo.id", "string", json!(j.cargo)),
            a("cargo", "string", json!(j.cargo)),
            a("cargo.mass", "float", json!(11635.0)),
            a("cargo.unit.count", "u32", json!(25)),
            a("destination.company.id", "string", json!("dst_co")),
            a("source.company.id", "string", json!("src_co")),
            a("destination.city.id", "string", json!(j.destination)),
            a("source.city.id", "string", json!(j.source)),
            a("income", "u64", json!(4314)),
            a("delivery.time", "u32", json!(j.deadline)),
            a("planned_distance.km", "u32", json!(j.planned_km)),
            a("cargo.loaded", "bool", json!(j.loaded)),
            a("job.market", "string", json!(j.market)),
            a("is.special.job", "bool", json!(false)),
        ])
    }

    fn cfg_job(&mut self) {
        let attrs = match self.job.clone() {
            Some(j) => self.job_attrs(&j),
            None => json!([]),
        };
        self.emit("config", json!({"id": "job", "attrs": attrs}));
    }

    fn cfg_truck(&mut self, empty: bool) {
        let attrs = if empty {
            json!([])
        } else {
            json!([{"name": "id", "index": null, "type": "string", "value": self.truck}])
        };
        self.emit("config", json!({"id": "truck", "attrs": attrs}));
    }

    fn boot(&mut self, first: bool) {
        self.sess_n += 1;
        self.sess = format!("{:032x}", 0xabc0_0000u64 + self.sess_n as u64);
        self.seq = 0;
        self.sim_us = 0;
        let gid = self.game;
        self.emit("init", json!({"game_id": gid, "game_version": {"major": 1, "minor": 7}, "api_version": "1.01"}));
        self.emit("config", json!({"id": "job", "attrs": []}));
        self.emit("config", json!({"id": "truck", "attrs": []}));
        let _ = first;
        // The world loads: configs re-sent, then timer_restart (PLAN §3).
        self.cfg_truck(false);
        self.cfg_job();
        self.emit("timer_restart", json!({"simulation_us": 16666, "render_us": 0}));
        self.sim_us = 16_666;
        self.paused = true;
        self.start();
        // The truck is sampled in the world before anything else happens.
        self.sample(0.0);
    }

    pub fn pause(&mut self) {
        if !self.paused {
            self.emit("paused", json!({}));
            self.paused = true;
        }
    }

    pub fn start(&mut self) {
        if self.paused {
            self.emit("started", json!({}));
            self.paused = false;
        }
    }

    pub fn sample(&mut self, speed_mps: f64) {
        let ch = json!({
            "truck.speed": speed_mps,
            "truck.odometer": self.odo as f32 as f64,
            "truck.fuel.amount": self.fuel,
            "truck.world.placement": {"position": {"x": self.pos[0], "y": self.pos[1], "z": self.pos[2]}},
            "game.time": self.gt as u64,
            "local.scale": self.scale,
            "multiplayer.time.offset": self.mp_offset,
            "trailer.0.connected": self.connected,
        });
        self.emit("sample", json!({"simulation_us": self.sim_us, "render_us": self.sim_us, "ch": ch, "stale": []}));
    }

    /// Drives for `secs` of simulation at `mph`, sampling at 20 Hz.
    pub fn drive_secs(&mut self, secs: f64, mph: f64) {
        self.start();
        let v = mph * MPS_PER_MPH;
        let steps = (secs * 20.0).round() as u64;
        for _ in 0..steps {
            self.sim_us += 50_000;
            self.wall_ms += 50;
            let world_m = v * 0.05;
            self.pos[0] += world_m;
            self.odo += world_m * self.scale / 1000.0;
            self.fuel -= world_m * 0.0004;
            self.gt += 0.05 * self.scale / 60.0;
            self.sample(v);
        }
    }

    /// Drives `game_km` on the odometer at 60 mph.
    pub fn drive_km(&mut self, game_km: f64) {
        let v = 60.0 * MPS_PER_MPH;
        let secs = game_km * 1000.0 / (v * self.scale);
        self.drive_secs(secs, 60.0);
    }

    pub fn idle(&mut self, secs: f64) {
        self.drive_secs(secs, 0.0);
    }

    pub fn save(&self) -> Snap {
        Snap {
            odo: self.odo,
            fuel: self.fuel,
            pos: self.pos,
            gt: self.gt,
            job: self.job.clone(),
            truck: self.truck.clone(),
            connected: self.connected,
        }
    }

    fn restore(&mut self, s: &Snap) {
        self.odo = s.odo;
        self.fuel = s.fuel;
        self.pos = s.pos;
        self.gt = s.gt;
        self.job = s.job.clone();
        self.truck = s.truck.clone();
        self.connected = s.connected;
    }

    /// In-session load (quick-load, autosave or save menu): W inside a pause.
    pub fn load(&mut self, s: &Snap) {
        self.pause();
        self.emit("config", json!({"id": "job", "attrs": []}));
        self.emit("config", json!({"id": "truck", "attrs": []}));
        self.restore(s);
        self.wall_ms += 15_000;
        self.cfg_truck(false);
        self.cfg_job();
        self.start();
    }

    /// The game is closed normally, restarted and continues from `s` (or
    /// from where it was when `s` is `None`).
    pub fn restart(&mut self, s: Option<&Snap>) {
        self.pause();
        self.emit("shutdown", json!({}));
        if let Some(s) = s {
            self.restore(s);
        }
        self.wall_ms += 60_000;
        self.boot(false);
    }

    /// The game is killed: no shutdown message.
    pub fn crash(&mut self, s: Option<&Snap>) {
        if let Some(s) = s {
            self.restore(s);
        }
        self.wall_ms += 60_000;
        self.boot(false);
    }

    pub fn accept(&mut self, j: JobSpec) {
        self.pause();
        self.job = Some(j);
        self.cfg_job();
        self.start();
    }

    pub fn load_cargo(&mut self) {
        self.pause();
        if let Some(j) = self.job.as_mut() {
            j.loaded = true;
        }
        self.gt += 48.0;
        self.cfg_job();
        self.cfg_truck(false);
        self.start();
    }

    /// `job.delivered` with the game's distance, then the job config empties.
    pub fn deliver(&mut self, distance_km: f64) {
        self.pause();
        let a = |n: &str, t: &str, v: Value| json!({"name": n, "index": null, "type": t, "value": v});
        let attrs = json!([
            a("revenue", "s64", json!(4314)),
            a("distance.km", "float", json!(distance_km)),
            a("delivery.time", "u32", json!(124)),
            a("auto.park.used", "bool", json!(true)),
        ]);
        self.emit("gameplay", json!({"id": "job.delivered", "attrs": attrs}));
        self.job = None;
        self.cfg_job();
        self.start();
    }

    /// Captured order: the job config empties first, then `job.cancelled`.
    pub fn cancel(&mut self) {
        self.pause();
        self.job = None;
        self.cfg_job();
        let attrs = json!([{"name": "cancel.penalty", "index": null, "type": "s64", "value": 12000}]);
        self.emit("gameplay", json!({"id": "job.cancelled", "attrs": attrs}));
        self.start();
    }

    /// F7, quick travel or a developer teleport: a jump inside a pause.
    pub fn teleport(&mut self, world_m: f64) {
        self.pause();
        self.pos[0] += world_m;
        self.start();
    }

    pub fn ferry(&mut self, world_m: f64) {
        self.pause();
        let attrs = json!([{"name": "source.id", "index": null, "type": "string", "value": "porttownsend"}]);
        self.emit("gameplay", json!({"id": "player.use.ferry", "attrs": attrs}));
        self.pos[0] += world_m;
        self.gt += 36.0;
        self.start();
    }

    pub fn switch_truck(&mut self, id: &str, odo: f64) {
        self.pause();
        self.truck = id.into();
        self.odo = odo;
        self.cfg_truck(false);
        self.start();
    }

    pub fn truckersmp(&mut self) {
        self.emit("modules", json!({"modules": ["KERNEL32.DLL", "core_atsmp.dll"]}));
    }

    /// The plugin's TruckersMP client SDK report (plugin/src/tmp.cpp).
    pub fn truckersmp_sdk(&mut self, connected: bool) {
        let server = if connected { 42 } else { 0 };
        self.emit("truckersmp", json!({"loaded": true, "connected": connected, "server_id": server, "account_id": 5557777}));
    }

    /// Client-inserted profile notice (PLAN §6b); not a plugin message, so no seq.
    pub fn profile(&mut self, key: &str) {
        self.lines.push(json!({"v": 1, "kind": "profile", "sess": self.sess, "wall_ms": self.wall_ms, "profile_key": key}).to_string());
    }

    /// A sample whose odometer moved by `game_km` (e.g. backwards) with no load.
    pub fn odometer_step(&mut self, game_km: f64) {
        self.odo += game_km;
        self.sim_us += 50_000;
        self.sample(0.0);
    }

    pub fn refuel(&mut self, litres: f64, l_per_s: f64) {
        self.start();
        let steps = (litres / l_per_s * 20.0).ceil() as u64;
        for _ in 0..steps {
            self.sim_us += 50_000;
            self.wall_ms += 50;
            self.fuel += l_per_s * 0.05;
            self.sample(0.0);
        }
    }

    pub fn replay_ctx(&self, ctx: &Context) -> Vec<Replayed> {
        let cfg = sample_config();
        let (frames, bad) = parse_lines(&self.lines, &cfg);
        assert_eq!(bad, 0);
        replay(&frames, &cfg, ctx)
    }

    pub fn replay(&self) -> Vec<Replayed> {
        self.replay_ctx(&Context::default())
    }

    /// The only job, or the job hauling `cargo`.
    pub fn job(&self, cargo: &str) -> (Job, JobResult) {
        let jobs = self.replay();
        let r = jobs
            .into_iter()
            .find(|r| r.job.info.fingerprint.cargo.cargo_id == cargo)
            .unwrap_or_else(|| panic!("no job hauling {cargo}"));
        (r.job, r.result)
    }
}

pub fn rescore(job: &Job, ctx: &Context) -> JobResult {
    evaluate(job, &sample_config(), ctx)
}
