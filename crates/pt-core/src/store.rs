//! Where a live timeline keeps its points (scorer memory, 2026-10-20).
//!
//! A job's kept timeline and each of its reverted branches is a *track*: an
//! ordered list of points behind a [`TrackId`]. The timeline only holds the ids;
//! the points live in a [`PointStore`]. The client keeps them in memory
//! ([`VecStore`]); the server's scorer keeps them on its Durable Object's disk,
//! so a job driven for days no longer has to fit in 128 MB.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::timeline::Point;

/// One track of a [`PointStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(pub u32);

/// Ordered point lists. Indexes are positions within a track, from 0.
pub trait PointStore {
    /// A new, empty track.
    fn create(&mut self) -> TrackId;
    /// Forgets a track and its points.
    fn remove(&mut self, t: TrackId);
    fn len(&self, t: TrackId) -> usize;
    fn get(&self, t: TrackId, i: usize) -> Option<Point>;
    fn push(&mut self, t: TrackId, p: Point);
    /// Moves the points from `at` on into a new track (empty when `at` is the
    /// length) and returns it.
    fn split_off(&mut self, t: TrackId, at: usize) -> TrackId;
    /// Moves every point of `src` to the end of `dst`; `src` is gone after.
    fn append(&mut self, dst: TrackId, src: TrackId);
    /// The points in order.
    fn iter(&self, t: TrackId) -> Box<dyn Iterator<Item = Point> + '_>;

    fn is_empty(&self, t: TrackId) -> bool {
        self.len(t) == 0
    }

    fn last(&self, t: TrackId) -> Option<Point> {
        self.len(t).checked_sub(1).and_then(|i| self.get(t, i))
    }

    /// Points that may lie within `radius` world metres of `pos`, with their
    /// index, in order. A store may return more (the caller still measures);
    /// it must not leave out a point that is within the radius.
    fn near(&self, t: TrackId, pos: [f64; 3], radius: f64) -> Box<dyn Iterator<Item = (usize, Point)> + '_> {
        let _ = (pos, radius);
        Box::new(self.iter(t).enumerate())
    }

    /// Index of the first point that satisfies `pred`.
    fn position(&self, t: TrackId, pred: &dyn Fn(&Point) -> bool) -> Option<usize> {
        self.iter(t).position(|p| pred(&p))
    }

    /// The whole track in memory.
    fn to_vec(&self, t: TrackId) -> Vec<Point> {
        self.iter(t).collect()
    }

    /// Gives back spare memory, where that means anything.
    fn shrink_to_fit(&mut self) {}
}

/// Every point in memory: the client, replays and tests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VecStore {
    next: u32,
    tracks: BTreeMap<TrackId, Vec<Point>>,
}

impl VecStore {
    pub fn new() -> VecStore {
        VecStore::default()
    }

    fn track(&self, t: TrackId) -> &[Point] {
        self.tracks.get(&t).map_or(&[], Vec::as_slice)
    }

    fn track_mut(&mut self, t: TrackId) -> &mut Vec<Point> {
        self.tracks.entry(t).or_default()
    }
}

impl PointStore for VecStore {
    fn create(&mut self) -> TrackId {
        let t = TrackId(self.next);
        self.next += 1;
        self.tracks.insert(t, Vec::new());
        t
    }

    fn remove(&mut self, t: TrackId) {
        self.tracks.remove(&t);
    }

    fn len(&self, t: TrackId) -> usize {
        self.track(t).len()
    }

    fn get(&self, t: TrackId, i: usize) -> Option<Point> {
        self.track(t).get(i).cloned()
    }

    fn push(&mut self, t: TrackId, p: Point) {
        self.track_mut(t).push(p);
    }

    fn split_off(&mut self, t: TrackId, at: usize) -> TrackId {
        let tail = self.track_mut(t).split_off(at);
        let n = self.create();
        *self.track_mut(n) = tail;
        n
    }

    fn append(&mut self, dst: TrackId, src: TrackId) {
        let mut moved = self.tracks.remove(&src).unwrap_or_default();
        self.track_mut(dst).append(&mut moved);
    }

    fn iter(&self, t: TrackId) -> Box<dyn Iterator<Item = Point> + '_> {
        Box::new(self.track(t).iter().cloned())
    }

    fn position(&self, t: TrackId, pred: &dyn Fn(&Point) -> bool) -> Option<usize> {
        self.track(t).iter().position(pred)
    }

    fn to_vec(&self, t: TrackId) -> Vec<Point> {
        self.track(t).to_vec()
    }

    fn shrink_to_fit(&mut self) {
        for v in self.tracks.values_mut() {
            v.shrink_to_fit();
        }
    }
}

/// Reads a job's point lists, whatever holds them: `Vec<Point>` (a finished,
/// stored job) through [`InMemory`], a [`TrackId`] through its store.
pub trait Resolve<P: ?Sized> {
    fn iter<'a>(&'a self, points: &'a P) -> Box<dyn Iterator<Item = Point> + 'a>;
}

/// Point lists that are plain vectors.
pub struct InMemory;

impl Resolve<Vec<Point>> for InMemory {
    fn iter<'a>(&'a self, points: &'a Vec<Point>) -> Box<dyn Iterator<Item = Point> + 'a> {
        Box::new(points.iter().cloned())
    }
}

impl<S: PointStore + ?Sized> Resolve<TrackId> for S {
    fn iter<'a>(&'a self, points: &'a TrackId) -> Box<dyn Iterator<Item = Point> + 'a> {
        PointStore::iter(self, *points)
    }
}

/// Points as compact bytes (a stored block or segment). Every field round-trips
/// bit for bit, so scores from decoded points are the same.
pub fn encode_points(points: &[Point]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + points.len() * 96);
    out.push(CODEC_VERSION);
    out.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for p in points {
        let flags: u16 = (p.odometer_km.is_some() as u16)
            | (p.fuel_l.is_some() as u16) << 1
            | (p.pos.is_some() as u16) << 2
            | (p.game_time_min.is_some() as u16) << 3
            | (p.cargo_loaded as u16) << 4
            | (p.trailer_connected as u16) << 5
            | (p.truckersmp as u16) << 6
            | (p.after_teleport as u16) << 7
            | (p.cargo_damage.is_some() as u16) << 8;
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&p.sess.to_le_bytes());
        out.extend_from_slice(&p.seq.to_le_bytes());
        out.extend_from_slice(&p.wall_ms.to_le_bytes());
        out.extend_from_slice(&p.sim_us.to_le_bytes());
        out.extend_from_slice(&p.run.to_le_bytes());
        out.extend_from_slice(&p.speed_mps.to_le_bytes());
        for v in [p.odometer_km, p.fuel_l].into_iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in p.pos.iter().flatten() {
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(t) = p.game_time_min {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out.extend_from_slice(&p.local_scale.to_le_bytes());
        out.extend_from_slice(&p.truck.to_le_bytes());
        out.extend_from_slice(&p.mp_time_offset.to_le_bytes());
        if let Some(d) = p.cargo_damage {
            out.extend_from_slice(&d.to_le_bytes());
        }
    }
    out
}

const CODEC_VERSION: u8 = 1;

pub fn decode_points(bytes: &[u8]) -> Result<Vec<Point>, String> {
    struct R<'a>(&'a [u8]);
    impl R<'_> {
        fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
            if self.0.len() < N {
                return Err("truncated point data".into());
            }
            let (a, b) = self.0.split_at(N);
            self.0 = b;
            Ok(a.try_into().expect("length checked"))
        }
        fn u16(&mut self) -> Result<u16, String> {
            self.take().map(u16::from_le_bytes)
        }
        fn u32(&mut self) -> Result<u32, String> {
            self.take().map(u32::from_le_bytes)
        }
        fn u64(&mut self) -> Result<u64, String> {
            self.take().map(u64::from_le_bytes)
        }
        fn i64(&mut self) -> Result<i64, String> {
            self.take().map(i64::from_le_bytes)
        }
        fn f64(&mut self) -> Result<f64, String> {
            self.take().map(f64::from_le_bytes)
        }
    }
    let mut r = R(bytes);
    let [version] = r.take()?;
    if version != CODEC_VERSION {
        return Err(format!("point data version {version}"));
    }
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let flags = r.u16()?;
        let bit = |k: u16| flags & (1 << k) != 0;
        let sess = r.u32()?;
        let seq = r.u64()?;
        let wall_ms = r.i64()?;
        let sim_us = r.u64()?;
        let run = r.u32()?;
        let speed_mps = r.f64()?;
        let odometer_km = if bit(0) { Some(r.f64()?) } else { None };
        let fuel_l = if bit(1) { Some(r.f64()?) } else { None };
        let pos = if bit(2) { Some([r.f64()?, r.f64()?, r.f64()?]) } else { None };
        let game_time_min = if bit(3) { Some(r.u32()?) } else { None };
        let local_scale = r.f64()?;
        let truck = r.u32()?;
        let mp_time_offset = r.i64()?;
        let cargo_damage = if bit(8) { Some(r.f64()?) } else { None };
        out.push(Point {
            sess,
            seq,
            wall_ms,
            sim_us,
            run,
            speed_mps,
            odometer_km,
            fuel_l,
            pos,
            game_time_min,
            local_scale,
            cargo_loaded: bit(4),
            trailer_connected: bit(5),
            truck,
            mp_time_offset,
            truckersmp: bit(6),
            after_teleport: bit(7),
            cargo_damage,
        });
    }
    if !r.0.is_empty() {
        return Err("trailing bytes after point data".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_codec_round_trips_every_field() {
        let mut a = pt(7);
        a.sess = 3;
        a.wall_ms = -12;
        a.odometer_km = Some(1234.5678);
        a.fuel_l = Some(f64::MIN_POSITIVE);
        a.game_time_min = Some(u32::MAX);
        a.cargo_loaded = true;
        a.truckersmp = true;
        a.after_teleport = true;
        a.cargo_damage = Some(0.125);
        a.speed_mps = -0.0;
        a.mp_time_offset = i64::MIN;
        let mut b = pt(8);
        b.pos = None;
        b.trailer_connected = false;
        let pts = vec![a, b];
        let back = decode_points(&encode_points(&pts)).unwrap();
        assert_eq!(back, pts);
        assert_eq!(back[0].speed_mps.to_bits(), (-0.0f64).to_bits());
        assert!(decode_points(&encode_points(&pts)[..20]).is_err());
    }

    fn pt(seq: u64) -> Point {
        Point {
            sess: 0,
            seq,
            wall_ms: seq as i64,
            sim_us: seq,
            run: 0,
            speed_mps: 0.0,
            odometer_km: None,
            fuel_l: None,
            pos: Some([seq as f64, 0.0, 0.0]),
            game_time_min: None,
            local_scale: 1.0,
            cargo_loaded: false,
            trailer_connected: true,
            truck: 0,
            mp_time_offset: 0,
            truckersmp: false,
            after_teleport: false,
            cargo_damage: None,
        }
    }

    fn seqs(s: &VecStore, t: TrackId) -> Vec<u64> {
        PointStore::iter(s, t).map(|p| p.seq).collect()
    }

    #[test]
    fn split_and_append_move_points() {
        let mut s = VecStore::new();
        let a = s.create();
        for i in 0..5 {
            s.push(a, pt(i));
        }
        let b = s.split_off(a, 3);
        assert_eq!((seqs(&s, a), seqs(&s, b)), (vec![0, 1, 2], vec![3, 4]));
        let empty = s.split_off(a, 3);
        assert!(s.is_empty(empty));
        s.append(a, b);
        assert_eq!(seqs(&s, a), vec![0, 1, 2, 3, 4]);
        assert_eq!(s.len(b), 0, "appended track is gone");
        assert_eq!(s.last(a).map(|p| p.seq), Some(4));
        assert_eq!(s.position(a, &|p| p.seq == 2), Some(2));
        assert_eq!(s.near(a, [0.0; 3], 1.0).map(|(i, _)| i).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }
}
