//! A scorer's points on its Durable Object's disk (scorer memory, 2026-10-20):
//! a [`PointStore`] over [`LocalSql`]. A job driven for days used to be
//! hundreds of MB in memory; here only one open block per track is.
//!
//! Points are stored in blocks of up to [`BLOCK`], one row each, compressed,
//! with the block's x/z bounding box so a load is matched against nearby
//! blocks only. A block is written when it fills and at [`SqlPoints::flush`];
//! the scorer flushes together with its state, between two awaits, so both
//! commit together.
//!
//! Storage errors are not expected (a Durable Object has GBs) and would leave
//! the timeline unusable, so they panic: the object restarts from its last
//! commit.

use std::cell::RefCell;
use std::collections::HashMap;

use pt_core::store::{decode_points, encode_points, PointStore, TrackId};
use pt_core::timeline::Point;

use crate::env::{LocalSql, SqlVal};
use crate::util;

/// Points per stored block: a row write per ~13 s of driving at 20 Hz.
pub const BLOCK: usize = 256;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pt_points (
    track INTEGER NOT NULL, block INTEGER NOT NULL, n INTEGER NOT NULL,
    min_x REAL, max_x REAL, min_z REAL, max_z REAL, data BLOB NOT NULL,
    PRIMARY KEY (track, block));
CREATE TABLE IF NOT EXISTS pt_kv (k TEXT PRIMARY KEY, v BLOB)";

/// Creates the store's tables (and the scorer's `pt_kv`) if missing.
pub fn ensure_schema<D: LocalSql>(db: &D) {
    for stmt in SCHEMA.split(';') {
        db.exec(stmt, &[]).unwrap_or_else(|e| panic!("point store schema: {e}"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BlockMeta {
    n: usize,
    /// min x, max x, min z, max z over the points that have a position.
    bbox: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Default)]
struct TrackMeta {
    blocks: Vec<BlockMeta>,
}

impl TrackMeta {
    fn len(&self) -> usize {
        self.blocks.iter().map(|b| b.n).sum()
    }

    /// Block and offset of point `i`.
    fn locate(&self, i: usize) -> Option<(usize, usize)> {
        let mut first = 0;
        for (b, m) in self.blocks.iter().enumerate() {
            if i < first + m.n {
                return Some((b, i - first));
            }
            first += m.n;
        }
        None
    }
}

fn bbox(points: &[Point]) -> Option<[f64; 4]> {
    points.iter().filter_map(|p| p.pos).fold(None, |acc, [x, _, z]| {
        Some(match acc {
            None => [x, x, z, z],
            Some([a, b, c, d]) => [a.min(x), b.max(x), c.min(z), d.max(z)],
        })
    })
}

fn opt(v: Option<f64>) -> SqlVal {
    v.map_or(SqlVal::Null, SqlVal::Real)
}

/// The points of a scorer. Serialises as nothing: the points are in the
/// database, and a restored scorer [`SqlPoints::attach`]es it again.
pub struct SqlPoints<D> {
    db: Option<D>,
    next: u32,
    meta: RefCell<HashMap<TrackId, TrackMeta>>,
    /// The last block of a track while points are appended to it; written when
    /// it fills or at a flush.
    tails: HashMap<TrackId, Vec<Point>>,
    /// The last block read, for neighbouring `get`s.
    last_read: RefCell<Option<(TrackId, usize, Vec<Point>)>>,
}

impl<D> Default for SqlPoints<D> {
    fn default() -> Self {
        SqlPoints { db: None, next: 0, meta: RefCell::new(HashMap::new()), tails: HashMap::new(), last_read: RefCell::new(None) }
    }
}

impl<D> serde::Serialize for SqlPoints<D> {
    fn serialize<Z: serde::Serializer>(&self, s: Z) -> Result<Z::Ok, Z::Error> {
        s.serialize_unit()
    }
}

impl<'de, D> serde::Deserialize<'de> for SqlPoints<D> {
    fn deserialize<Z: serde::Deserializer<'de>>(d: Z) -> Result<Self, Z::Error> {
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(SqlPoints::default())
    }
}

impl<D: LocalSql> SqlPoints<D> {
    /// Opens the store in `db`, creating its tables.
    pub fn open(db: D) -> SqlPoints<D> {
        let mut s = SqlPoints::default();
        s.attach(db);
        s
    }

    /// Connects a deserialised store to its database again.
    pub fn attach(&mut self, db: D) {
        ensure_schema(&db);
        let next = db.exec("SELECT v FROM pt_kv WHERE k = 'next_track'", &[]).expect("point store");
        self.next = next.into_iter().next().map_or(0, |mut r| r.remove(0).int() as u32);
        self.db = Some(db);
        self.meta.borrow_mut().clear();
        self.tails.clear();
        *self.last_read.borrow_mut() = None;
    }

    fn db(&self) -> &D {
        self.db.as_ref().expect("point store attached")
    }

    fn exec(&self, sql: &str, params: &[SqlVal]) -> Vec<Vec<SqlVal>> {
        self.db().exec(sql, params).unwrap_or_else(|e| panic!("point store: {e}"))
    }

    /// Forgets every point (a rebuild).
    pub fn clear(&mut self) {
        self.exec("DELETE FROM pt_points", &[]);
        self.exec("DELETE FROM pt_kv WHERE k = 'next_track'", &[]);
        self.next = 0;
        self.meta.borrow_mut().clear();
        self.tails.clear();
        *self.last_read.borrow_mut() = None;
    }

    /// Writes every open block and the track counter.
    pub fn flush(&mut self) {
        let tails: Vec<TrackId> = self.tails.keys().copied().collect();
        for t in tails {
            self.write_tail(t);
        }
        self.exec("INSERT OR REPLACE INTO pt_kv (k, v) VALUES ('next_track', ?1)", &[SqlVal::Int(self.next as i64)]);
    }

    /// Bytes of SQLite data the points take (rough: compressed blocks).
    pub fn stored_bytes(&self) -> u64 {
        let r = self.exec("SELECT COALESCE(SUM(LENGTH(data)), 0) FROM pt_points", &[]);
        r.first().map_or(0, |r| r[0].int() as u64)
    }

    /// Points held in memory right now (open blocks and the read cache).
    pub fn points_in_memory(&self) -> usize {
        self.tails.values().map(Vec::len).sum::<usize>() + self.last_read.borrow().as_ref().map_or(0, |(_, _, v)| v.len())
    }

    fn write_block(&self, t: TrackId, b: usize, points: &[Point]) {
        let bb = bbox(points);
        let data = util::gzip(&encode_points(points));
        self.exec(
            "INSERT OR REPLACE INTO pt_points (track, block, n, min_x, max_x, min_z, max_z, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            &[
                SqlVal::Int(t.0 as i64),
                SqlVal::Int(b as i64),
                SqlVal::Int(points.len() as i64),
                opt(bb.map(|v| v[0])),
                opt(bb.map(|v| v[1])),
                opt(bb.map(|v| v[2])),
                opt(bb.map(|v| v[3])),
                SqlVal::Blob(data),
            ],
        );
    }

    /// Writes and closes a track's open block.
    fn write_tail(&mut self, t: TrackId) {
        let Some(tail) = self.tails.remove(&t) else { return };
        let b = self.with_meta(t, |m| m.blocks.len()) - 1;
        if tail.is_empty() {
            self.exec("DELETE FROM pt_points WHERE track = ?1 AND block = ?2", &[SqlVal::Int(t.0 as i64), SqlVal::Int(b as i64)]);
            self.meta.borrow_mut().get_mut(&t).expect("loaded").blocks.pop();
        } else {
            self.write_block(t, b, &tail);
        }
    }

    fn with_meta<R>(&self, t: TrackId, f: impl FnOnce(&mut TrackMeta) -> R) -> R {
        let mut cache = self.meta.borrow_mut();
        let m = cache.entry(t).or_insert_with(|| {
            let rows = self.exec(
                "SELECT n, min_x, max_x, min_z, max_z FROM pt_points WHERE track = ?1 ORDER BY block",
                &[SqlVal::Int(t.0 as i64)],
            );
            TrackMeta {
                blocks: rows
                    .into_iter()
                    .map(|r| {
                        let bbox = match (r[1].real(), r[2].real(), r[3].real(), r[4].real()) {
                            (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
                            _ => None,
                        };
                        BlockMeta { n: r[0].int() as usize, bbox }
                    })
                    .collect(),
            }
        });
        f(m)
    }

    fn read_block(&self, t: TrackId, b: usize) -> Vec<Point> {
        if let Some(tail) = self.tails.get(&t) {
            if self.with_meta(t, |m| m.blocks.len()) == b + 1 {
                return tail.clone();
            }
        }
        if let Some((rt, rb, pts)) = self.last_read.borrow().as_ref() {
            if *rt == t && *rb == b {
                return pts.clone();
            }
        }
        let rows = self.exec("SELECT data FROM pt_points WHERE track = ?1 AND block = ?2", &[SqlVal::Int(t.0 as i64), SqlVal::Int(b as i64)]);
        let data = rows.into_iter().next().map(|mut r| r.remove(0).into_bytes()).unwrap_or_default();
        let raw = util::gunzip(&data, usize::MAX).unwrap_or_else(|e| panic!("point block {t:?}/{b}: {e}"));
        let pts = decode_points(&raw).unwrap_or_else(|e| panic!("point block {t:?}/{b}: {e}"));
        *self.last_read.borrow_mut() = Some((t, b, pts.clone()));
        pts
    }

    /// Makes the database authoritative for `t` (before moving its blocks).
    fn settle(&mut self, t: TrackId) {
        self.write_tail(t);
        self.meta.borrow_mut().remove(&t);
        *self.last_read.borrow_mut() = None;
    }
}

struct TrackIter<'a, D: LocalSql> {
    store: &'a SqlPoints<D>,
    t: TrackId,
    blocks: Vec<usize>,
    next_block: usize,
    /// Index of the first point of `buf` in the track, per block.
    firsts: Vec<usize>,
    buf: std::iter::Enumerate<std::vec::IntoIter<Point>>,
    base: usize,
}

impl<D: LocalSql> Iterator for TrackIter<'_, D> {
    type Item = (usize, Point);

    fn next(&mut self) -> Option<(usize, Point)> {
        loop {
            if let Some((k, p)) = self.buf.next() {
                return Some((self.base + k, p));
            }
            let b = *self.blocks.get(self.next_block)?;
            self.next_block += 1;
            self.base = self.firsts[b];
            self.buf = self.store.read_block(self.t, b).into_iter().enumerate();
        }
    }
}

impl<D: LocalSql> SqlPoints<D> {
    /// Points of the given blocks of `t`, with their index, in order.
    fn blocks_iter(&self, t: TrackId, keep: impl Fn(&BlockMeta) -> bool) -> TrackIter<'_, D> {
        let (blocks, firsts) = self.with_meta(t, |m| {
            let mut firsts = Vec::with_capacity(m.blocks.len());
            let mut first = 0;
            for b in &m.blocks {
                firsts.push(first);
                first += b.n;
            }
            let blocks = m.blocks.iter().enumerate().filter(|(_, b)| keep(b)).map(|(i, _)| i).collect();
            (blocks, firsts)
        });
        TrackIter { store: self, t, blocks, next_block: 0, firsts, buf: Vec::new().into_iter().enumerate(), base: 0 }
    }
}

impl<D: LocalSql> PointStore for SqlPoints<D> {
    fn create(&mut self) -> TrackId {
        let t = TrackId(self.next);
        self.next += 1;
        self.meta.borrow_mut().insert(t, TrackMeta::default());
        t
    }

    fn remove(&mut self, t: TrackId) {
        self.tails.remove(&t);
        self.meta.borrow_mut().remove(&t);
        *self.last_read.borrow_mut() = None;
        self.exec("DELETE FROM pt_points WHERE track = ?1", &[SqlVal::Int(t.0 as i64)]);
    }

    fn len(&self, t: TrackId) -> usize {
        self.with_meta(t, |m| m.len())
    }

    fn get(&self, t: TrackId, i: usize) -> Option<Point> {
        let (b, o) = self.with_meta(t, |m| m.locate(i))?;
        self.read_block(t, b).into_iter().nth(o)
    }

    fn push(&mut self, t: TrackId, p: Point) {
        if !self.tails.contains_key(&t) {
            // Reopen the last block if it has room, else start one.
            let last = self.with_meta(t, |m| m.blocks.last().copied().map(|b| (m.blocks.len() - 1, b.n)));
            let tail = match last {
                Some((b, n)) if n < BLOCK => self.read_block(t, b),
                _ => {
                    self.with_meta(t, |m| m.blocks.push(BlockMeta { n: 0, bbox: None }));
                    Vec::new()
                }
            };
            self.tails.insert(t, tail);
        }
        let tail = self.tails.get_mut(&t).expect("just opened");
        tail.push(p);
        let (n, bb) = (tail.len(), bbox(tail));
        self.with_meta(t, |m| *m.blocks.last_mut().expect("open block") = BlockMeta { n, bbox: bb });
        let stale = matches!(self.last_read.borrow().as_ref(), Some((rt, _, _)) if *rt == t);
        if stale {
            *self.last_read.borrow_mut() = None;
        }
        if n >= BLOCK {
            self.write_tail(t);
        }
    }

    fn split_off(&mut self, t: TrackId, at: usize) -> TrackId {
        self.settle(t);
        let n = self.create();
        let Some((b, o)) = self.with_meta(t, |m| m.locate(at)) else { return n };
        let (ti, ni, bi) = (SqlVal::Int(t.0 as i64), SqlVal::Int(n.0 as i64), SqlVal::Int(b as i64));
        if o == 0 {
            self.exec("UPDATE pt_points SET track = ?1, block = block - ?2 WHERE track = ?3 AND block >= ?2", &[ni, bi, ti]);
        } else {
            let mut head = self.read_block(t, b);
            let rest = head.split_off(o);
            self.exec("UPDATE pt_points SET track = ?1, block = block - ?2 WHERE track = ?3 AND block > ?2", &[ni, bi, ti]);
            self.write_block(t, b, &head);
            self.write_block(n, 0, &rest);
        }
        self.meta.borrow_mut().remove(&t);
        self.meta.borrow_mut().remove(&n);
        *self.last_read.borrow_mut() = None;
        n
    }

    fn append(&mut self, dst: TrackId, src: TrackId) {
        self.settle(dst);
        self.settle(src);
        let k = self.with_meta(dst, |m| m.blocks.len());
        self.exec(
            "UPDATE pt_points SET track = ?1, block = block + ?2 WHERE track = ?3",
            &[SqlVal::Int(dst.0 as i64), SqlVal::Int(k as i64), SqlVal::Int(src.0 as i64)],
        );
        self.meta.borrow_mut().remove(&dst);
        self.meta.borrow_mut().remove(&src);
        *self.last_read.borrow_mut() = None;
    }

    fn iter(&self, t: TrackId) -> Box<dyn Iterator<Item = Point> + '_> {
        Box::new(self.blocks_iter(t, |_| true).map(|(_, p)| p))
    }

    fn near(&self, t: TrackId, pos: [f64; 3], radius: f64) -> Box<dyn Iterator<Item = (usize, Point)> + '_> {
        let (x, z) = (pos[0], pos[2]);
        Box::new(self.blocks_iter(t, move |b| {
            b.bbox.is_some_and(|[x0, x1, z0, z1]| x0 - radius <= x && x <= x1 + radius && z0 - radius <= z && z <= z1 + radius)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(i: u64, x: f64) -> Point {
        Point {
            sess: 0,
            seq: i,
            wall_ms: i as i64,
            sim_us: i,
            run: 0,
            speed_mps: 1.0,
            odometer_km: Some(i as f64),
            fuel_l: None,
            pos: Some([x, 0.0, 0.0]),
            game_time_min: None,
            local_scale: 1.0,
            cargo_loaded: true,
            trailer_connected: true,
            truck: 0,
            mp_time_offset: 0,
            truckersmp: false,
            after_teleport: false,
            cargo_damage: None,
        }
    }

    #[test]
    fn bbox_covers_positions_only() {
        let mut a = pt(0, 5.0);
        a.pos = None;
        assert_eq!(bbox(&[a.clone()]), None);
        assert_eq!(bbox(&[a, pt(1, -2.0), pt(2, 3.0)]), Some([-2.0, 3.0, 0.0, 0.0]));
    }
}
