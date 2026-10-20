//! Route GeoJSON (PUBLIC_API §4, ADMIN_API §5): a job's points projected to
//! WGS84 with `pt_core::projection`, split into features wherever the bot should
//! draw differently.

use pt_core::projection;
use pt_core::timeline::{Job, JobEvent, Point};
use pt_core::Config;
use serde_json::{json, Value};

use crate::env::{Host, RowExt};
use crate::http::{ApiResult, Request, Response};
use crate::jobs::class_name;
use crate::public::job_row;
use crate::util::{rfc3339, round2};
use crate::{admin, auth, config};

pub const MAX_VERTICES: usize = 5000;

/// A pause: the wall clock ran this much further than the simulation.
const PAUSE_MS: i64 = 5_000;

/// Jumps between runs shorter than this (game metres) are no break worth drawing:
/// a load lands where the save was made.
const CONNECT_M: f64 = 50.0;

/// Douglas–Peucker tolerances tried in turn (game metres) until the route fits.
const TOLERANCES_M: [f64; 12] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Kept,
    Restored,
    Reverted,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Kept => "kept",
            Status::Restored => "restored",
            Status::Reverted => "reverted",
        }
    }
}

struct Seg {
    kind: &'static str,
    status: Status,
    /// Game X/Z.
    pts: Vec<[f64; 2]>,
    scale: Option<f64>,
    start: i64,
    end: i64,
    counted: bool,
    game_km: f64,
    world_km: f64,
}

/// Game metres per degree in the flat local projection (one degree of
/// latitude), so a 1:1 map keeps its real size.
const LOCAL_M_PER_DEG: f64 = 111_320.0;

/// `Some(centre)` (game x, z) when any point of the route falls outside the
/// basemap projection's range: a 1:1 map mod's distances, read at the vanilla
/// map's scale, wrap around the globe (the owner's Alaska job, 2026-10-20:
/// longitudes down to -332°). The whole route is then drawn flat in game
/// metres around that centre.
fn local_centre(job: &Job, shapes: &[Vec<[f64; 2]>]) -> Option<[f64; 2]> {
    let pts = || shapes.iter().flatten();
    let off = pts().any(|p| {
        let [lon, lat] = projection::to_lon_lat(job.game, p[0], p[1]);
        !(lon.is_finite() && lat.is_finite() && (-180.0..=180.0).contains(&lon) && (-85.0..=85.0).contains(&lat))
    });
    if !off {
        return None;
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in pts() {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    Some([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0])
}

fn xz(p: &Point) -> Option<[f64; 2]> {
    p.pos.map(|q| [q[0], q[2]])
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Kept points restored by a load (`LoadRestore`): the ones right after the fork.
fn kept_status(job: &Job) -> Vec<Status> {
    let mut st = vec![Status::Kept; job.kept.len()];
    for e in &job.events {
        if let JobEvent::LoadRestore { from_fork_seq, restored_points, .. } = e {
            if let Some(i) = job.kept.iter().position(|p| p.seq == *from_fork_seq) {
                for s in st.iter_mut().skip(i + 1).take(*restored_points) {
                    *s = Status::Restored;
                }
            }
        }
    }
    st
}

/// What lies between two points that are not neighbours on one drive.
fn connector(job: &Job, a: &Point, b: &Point, pa: [f64; 2], pb: [f64; 2]) -> Option<Seg> {
    let seg = |kind, counted, game_km: f64, world_km| Seg {
        kind, status: Status::Kept, pts: vec![pa, pb], scale: None, start: a.wall_ms, end: b.wall_ms, counted, game_km, world_km,
    };
    if b.after_teleport {
        let via = job.events.iter().find_map(|e| match e {
            JobEvent::Teleport { ferry_or_train, via, wall_ms, .. } if *wall_ms == b.wall_ms => Some((*ferry_or_train, via.clone())),
            _ => None,
        });
        let kind = match via {
            Some((true, Some(v))) if v == "ferry" => "ferry",
            Some((true, Some(v))) if v == "train" => "train",
            Some((true, _)) => "ferry_or_train",
            _ => "teleport",
        };
        return Some(seg(kind, false, 0.0, 0.0));
    }
    if let Some(g) = job.active_gaps().find(|g| g.to_sess == b.sess && g.to_seq == b.seq) {
        let km = g.game_km.unwrap_or(0.0).max(0.0);
        return Some(seg("gap", g.counted, km, km / g.local_scale));
    }
    (dist(pa, pb) > CONNECT_M).then(|| seg("teleport", false, 0.0, 0.0))
}

/// Splits one ordered run of points into driving features and connectors.
fn segments(job: &Job, points: &[(&Point, Status)], admin: bool, out: &mut Vec<Seg>) {
    let wot = job.info.is_world_of_trucks();
    let pts: Vec<(&Point, Status, [f64; 2])> = points.iter().filter_map(|(p, s)| xz(p).map(|q| (*p, *s, q))).collect();
    let mut cur: Option<Seg> = None;
    for w in pts.windows(2) {
        let ((a, _, pa), (b, sb, pb)) = (w[0], w[1]);
        let counted = wot || (b.cargo_loaded && b.trailer_connected);
        let same_drive = a.run == b.run && a.truck == b.truck && !b.after_teleport;
        if !same_drive {
            out.extend(cur.take());
            if let Some(mut c) = connector(job, a, b, pa, pb) {
                c.status = sb;
                out.push(c);
            }
            continue;
        }
        let paused = a.sess == b.sess && (b.wall_ms - a.wall_ms) - (b.sim_us.saturating_sub(a.sim_us) / 1000) as i64 > PAUSE_MS;
        let split = cur.as_ref().is_some_and(|c| {
            paused || c.scale != Some(b.local_scale) || c.counted != counted || (admin && c.status != sb)
        });
        if split {
            out.extend(cur.take());
        }
        let c = cur.get_or_insert_with(|| Seg {
            kind: "driving", status: sb, pts: vec![pa], scale: Some(b.local_scale), start: a.wall_ms, end: a.wall_ms,
            counted, game_km: 0.0, world_km: 0.0,
        });
        c.pts.push(pb);
        c.end = b.wall_ms;
        if let (Some(x), Some(y)) = (a.odometer_km, b.odometer_km) {
            let km = (y - x).max(0.0);
            c.game_km += km;
            c.world_km += km / b.local_scale;
        }
    }
    out.extend(cur);
}

/// Douglas–Peucker: the indices of the points to keep.
fn simplify(pts: &[[f64; 2]], tol: f64) -> Vec<[f64; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        let (a, b) = (pts[i], pts[j]);
        let len = dist(a, b);
        let (mut far, mut at) = (0.0, 0);
        for (k, p) in pts.iter().enumerate().take(j).skip(i + 1) {
            let d = if len == 0.0 {
                dist(*p, a)
            } else {
                ((b[0] - a[0]) * (a[1] - p[1]) - (a[0] - p[0]) * (b[1] - a[1])).abs() / len
            };
            if d > far {
                (far, at) = (d, k);
            }
        }
        if far > tol {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

fn r6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The FeatureCollection for a job; `admin` adds reverted material and statuses.
pub fn geojson(job: &Job, cfg: &Config, job_id: &str, admin: bool) -> Value {
    let status = kept_status(job);
    let kept: Vec<(&Point, Status)> = job.kept.iter().zip(status.iter().copied()).collect();
    let mut segs = Vec::new();
    segments(job, &kept, admin, &mut segs);
    if admin {
        for br in &job.reverted {
            let fork = job.kept.iter().find(|p| p.seq == br.fork_seq);
            let pts: Vec<(&Point, Status)> = fork.into_iter().chain(br.points.iter()).map(|p| (p, Status::Reverted)).collect();
            let mut s = Vec::new();
            segments(job, &pts, true, &mut s);
            for x in &mut s {
                x.status = Status::Reverted;
            }
            segs.extend(s);
        }
        segs.sort_by_key(|s| (s.start, s.status == Status::Reverted));
    }

    // Simplify the driving features until the whole route fits.
    let fixed: usize = segs.iter().filter(|s| s.kind != "driving").map(|s| s.pts.len()).sum();
    let mut tol = 0.0;
    let mut shapes: Vec<Vec<[f64; 2]>> = segs.iter().map(|s| s.pts.clone()).collect();
    for t in TOLERANCES_M {
        tol = t;
        shapes = segs.iter().map(|s| if s.kind == "driving" { simplify(&s.pts, t) } else { s.pts.clone() }).collect();
        if shapes.iter().map(Vec::len).sum::<usize>() <= MAX_VERTICES.max(fixed) {
            break;
        }
    }

    let vanilla = cfg.map.scale.vanilla(job.game);
    let local = local_centre(job, &shapes);
    let project = |p: &[f64; 2]| match local {
        Some([cx, cz]) => [(p[0] - cx) / LOCAL_M_PER_DEG, -(p[1] - cz) / LOCAL_M_PER_DEG],
        None => projection::to_lon_lat(job.game, p[0], p[1]),
    };
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut features = Vec::new();
    let mut vertices = 0;
    for (i, (s, shape)) in segs.iter().zip(&shapes).enumerate() {
        let coords: Vec<Value> = shape
            .iter()
            .map(|p| {
                let [lon, lat] = project(p);
                lo = [lo[0].min(lon), lo[1].min(lat)];
                hi = [hi[0].max(lon), hi[1].max(lat)];
                json!([r6(lon), r6(lat)])
            })
            .collect();
        vertices += coords.len();
        let mut props = json!({
            "index": i, "segment": s.kind, "local_scale": s.scale,
            "scale_class": class_name(vanilla.classify(s.scale)),
            "start_at": rfc3339(s.start), "end_at": rfc3339(s.end),
            "counted": s.counted, "world_km": round2(s.world_km), "game_km": round2(s.game_km),
        });
        if admin {
            props["status"] = s.status.as_str().into();
        }
        features.push(json!({"type": "Feature", "geometry": {"type": "LineString", "coordinates": coords}, "properties": props}));
    }
    let bbox = if vertices > 0 { json!([r6(lo[0]), r6(lo[1]), r6(hi[0]), r6(hi[1])]) } else { Value::Null };
    json!({
        "type": "FeatureCollection",
        "bbox": bbox,
        "properties": {
            "job_id": job_id, "game": job.game.as_str(), "vertex_count": vertices, "max_vertices": MAX_VERTICES,
            // Game metres at the main road scale, as 1:1 metres.
            "simplification_tolerance_world_m": crate::util::round3(tol / vanilla.main_road_local_scale()),
            "includes_reverted": admin,
            // "basemap": lines up with the game's basemap tiles. "local": the
            // route left the basemap's projection (a 1:1 map mod), so it is
            // drawn flat in game metres around 0°, 0°, off any tiles.
            "projection": if local.is_some() { "local" } else { "basemap" },
        },
        "features": features,
    })
}

async fn respond<H: Host>(h: &H, id: &str, admin_view: bool) -> ApiResult<Response> {
    let row = job_row(h, id).await?;
    let job_id = row.s("job_id");
    let (job, _) = admin::load_job(h, &job_id).await?;
    let cfg = config::scoring(h).await?.cfg;
    let v = geojson(&job, &cfg, &job_id, admin_view);
    Ok(Response::bytes(200, "application/geo+json", v.to_string().into_bytes()))
}

pub async fn public<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    respond(h, id, false).await
}

pub async fn admin<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    respond(h, id, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn douglas_peucker() {
        let line: Vec<[f64; 2]> = (0..=100).map(|i| [i as f64, 0.0]).collect();
        assert_eq!(simplify(&line, 1.0), vec![[0.0, 0.0], [100.0, 0.0]]);
        let bend = vec![[0.0, 0.0], [5.0, 0.1], [10.0, 0.0], [10.0, 10.0]];
        assert_eq!(simplify(&bend, 1.0), vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]);
        assert_eq!(simplify(&bend, 0.01).len(), 4);
    }
}
