//! Small self-contained helpers: UTC dates, IDs, base64url, HMAC-SHA256, gzip.

use std::io::{Read, Write};

use sha2::{Digest, Sha256};

// ---- dates (UTC, proleptic Gregorian) ----------------------------------------

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const DAY_MS: i64 = 86_400_000;

/// `2026-10-13T21:25:00.070Z` (API_CONVENTIONS §6).
pub fn rfc3339(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(DAY_MS), ms.rem_euclid(DAY_MS));
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s, milli) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

pub fn opt_rfc3339(ms: Option<i64>) -> serde_json::Value {
    ms.map_or(serde_json::Value::Null, |m| rfc3339(m).into())
}

/// Parses RFC 3339 with a `Z` or numeric offset and optional fraction.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut milli = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = &s[start..i];
        if frac.is_empty() {
            return None;
        }
        milli = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    }
    let offset_min = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(&c @ (b'+' | b'-')) if i + 6 == b.len() && b[i + 3] == b':' => {
            let v = num(i + 1..i + 3)? * 60 + num(i + 4..i + 6)?;
            if c == b'+' {
                v
            } else {
                -v
            }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some(days * DAY_MS + h * 3_600_000 + mi * 60_000 + sec * 1000 + milli - offset_min * 60_000)
}

/// Monday 00:00 UTC of ISO week `week` of ISO year `year`.
pub fn iso_week_start(year: i64, week: u32) -> i64 {
    // The week with January 4th is week 1.
    let jan4 = days_from_civil(year, 1, 4);
    let weekday = (jan4 + 3).rem_euclid(7); // 0 = Monday
    (jan4 - weekday + (week as i64 - 1) * 7) * DAY_MS
}

/// ISO year and week of an instant.
pub fn iso_week_of(ms: i64) -> (i64, u32) {
    let days = ms.div_euclid(DAY_MS);
    let weekday = (days + 3).rem_euclid(7);
    let thursday = days - weekday + 3;
    let (y, _, _) = civil_from_days(thursday);
    let week = ((thursday - days_from_civil(y, 1, 1)) / 7 + 1) as u32;
    (y, week)
}

pub fn iso_weeks_in_year(year: i64) -> u32 {
    iso_week_of(days_from_civil(year, 12, 28) * DAY_MS).1
}

pub fn month_start(year: i64, month: u32) -> i64 {
    days_from_civil(year, month, 1) * DAY_MS
}

pub fn year_month_of(ms: i64) -> (i64, u32) {
    let (y, m, _) = civil_from_days(ms.div_euclid(DAY_MS));
    (y, m)
}

// ---- encodings -------------------------------------------------------------------

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding.
pub fn b64url(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(B64URL[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = B64URL.iter().position(|&x| x == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Crockford base32 (no I, L, O, U).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// `n` Crockford characters from random bytes (5 bits each).
pub fn crockford(random: &[u8], n: usize) -> String {
    let mut bits = random.iter().flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1));
    (0..n)
        .map(|_| {
            let v = (0..5).fold(0usize, |acc, _| acc << 1 | bits.next().unwrap_or(0) as usize);
            CROCKFORD[v] as char
        })
        .collect()
}

/// A job id as typed by a person: case-insensitive, optional hyphen, I/L → 1
/// and O → 0 (API_CONVENTIONS §6). `None` unless 8 valid characters remain.
pub fn normalize_job_id(s: &str) -> Option<String> {
    let id: String = s
        .chars()
        .filter(|c| *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        })
        .collect();
    (id.len() == 8 && id.bytes().all(|b| CROCKFORD.contains(&b))).then_some(id)
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.iter().map(|b| b ^ x).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

/// Compares in time independent of where the inputs differ.
pub fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn gunzip(data: &[u8], limit: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data).take(limit as u64 + 1).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(std::io::Error::other("too large"));
    }
    Ok(out)
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).expect("in-memory write");
    enc.finish().expect("in-memory finish")
}

/// km → miles, the contract's two-decimal rounding for shown distances.
pub fn mi(km: f64) -> f64 {
    round2(km / 1.609_344)
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        let ms = parse_rfc3339("2026-10-13T21:25:00.070Z").unwrap();
        assert_eq!(rfc3339(ms), "2026-10-13T21:25:00.070Z");
        assert_eq!(parse_rfc3339("2026-10-13T23:25:00+02:00"), parse_rfc3339("2026-10-13T21:25:00Z"));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59.999Z");
        assert!(parse_rfc3339("2026-13-01T00:00:00Z").is_none());
        assert!(parse_rfc3339("2026-10-13").is_none());
    }

    #[test]
    fn iso_weeks() {
        // 2026-10-14 is a Wednesday of ISO week 42; the week starts Monday 12th.
        let ms = parse_rfc3339("2026-10-14T09:00:00Z").unwrap();
        assert_eq!(iso_week_of(ms), (2026, 42));
        assert_eq!(rfc3339(iso_week_start(2026, 42)), "2026-10-12T00:00:00.000Z");
        // 2027-01-01 (Friday) is still in 2026-W53.
        assert_eq!(iso_week_of(parse_rfc3339("2027-01-01T12:00:00Z").unwrap()), (2026, 53));
        assert_eq!(iso_weeks_in_year(2026), 53);
        assert_eq!(iso_weeks_in_year(2025), 52);
        assert_eq!(rfc3339(iso_week_start(2026, 1)), "2025-12-29T00:00:00.000Z");
    }

    #[test]
    fn encodings() {
        assert_eq!(b64url(b"hello?"), "aGVsbG8_");
        assert_eq!(b64url(b"ab"), "YWI");
        assert_eq!(b64url_decode("YWI").unwrap(), b"ab");
        assert_eq!(crockford(&[0xff; 5], 8), "ZZZZZZZZ");
        assert_eq!(crockford(&[0; 5], 8), "00000000");
        assert_eq!(normalize_job_id("7k2m-9qxa").as_deref(), Some("7K2M9QXA"));
        assert_eq!(normalize_job_id("OIL00000").as_deref(), Some("01100000"));
        assert_eq!(normalize_job_id("7K2M9QX"), None);
        assert_eq!(normalize_job_id("7K2M9QXU"), None);
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex(&mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn gzip_round_trip_and_limit() {
        let z = gzip(b"abcabcabc");
        assert_eq!(gunzip(&z, 100).unwrap(), b"abcabcabc");
        assert!(gunzip(&z, 5).is_err());
    }
}
