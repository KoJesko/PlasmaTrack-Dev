//! Host-independent HTTP types, the error shape (API_CONVENTIONS §4) and
//! cursors (§5).

use serde_json::{json, Value};

use crate::env::HostError;
use crate::util;

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// Path without the query string, percent-decoding not applied.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, url_path: &str) -> Request {
        let (path, q) = url_path.split_once('?').unwrap_or((url_path, ""));
        Request { method: method.to_string(), path: path.to_string(), query: parse_query(q), ..Default::default() }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Request {
        self.headers.push((k.to_ascii_lowercase(), v.to_string()));
        self
    }

    pub fn with_json(mut self, v: &Value) -> Request {
        self.body = v.to_string().into_bytes();
        self.with_header("content-type", "application/json")
    }

    pub fn q(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn json_body(&self) -> Result<Value, ApiError> {
        if self.body.is_empty() {
            return Ok(json!({}));
        }
        let v: Value = serde_json::from_slice(&self.body).map_err(|e| ApiError::bad_request(&format!("malformed JSON: {e}")))?;
        if !v.is_object() {
            return Err(ApiError::bad_request("the body must be a JSON object"));
        }
        Ok(v)
    }
}

fn hexval(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hexval(b[i + 1]), hexval(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, v: &Value) -> Response {
        Response {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: v.to_string().into_bytes(),
        }
    }

    pub fn ok(v: Value) -> Response {
        Response::json(200, &v)
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Response {
        Response { status, headers: vec![("content-type".into(), content_type.into())], body }
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Response {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn body_json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A non-2xx answer with the contract's error body.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub details: Value,
    pub retry_after_s: Option<u64>,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: &str) -> ApiError {
        ApiError { status, code, message: message.to_string(), details: json!({}), retry_after_s: None }
    }
    pub fn details(mut self, d: Value) -> ApiError {
        self.details = d;
        self
    }
    pub fn bad_request(m: &str) -> ApiError {
        ApiError::new(400, "bad_request", m)
    }
    pub fn unauthorized() -> ApiError {
        ApiError::new(401, "unauthorized", "Missing or unknown token.")
    }
    pub fn forbidden(m: &str) -> ApiError {
        ApiError::new(403, "forbidden", m)
    }
    pub fn not_found(resource: &str) -> ApiError {
        ApiError::new(404, "not_found", &format!("No such {resource}.")).details(json!({"resource": resource}))
    }
    pub fn invalid_transition(from: &str, action: &str, m: &str) -> ApiError {
        ApiError::new(409, "invalid_transition", m).details(json!({"from": from, "action": action}))
    }
    pub fn conflict(m: &str, details: Value) -> ApiError {
        ApiError::new(409, "conflict", m).details(details)
    }
    pub fn validation(fields: Value) -> ApiError {
        ApiError::new(422, "validation_failed", "Some fields are invalid.").details(json!({ "fields": fields }))
    }
    pub fn field(name: &str, problem: &str) -> ApiError {
        ApiError::validation(json!({ name: problem }))
    }
    pub fn too_large(max_bytes: usize) -> ApiError {
        ApiError::new(413, "payload_too_large", "The request body is too large.").details(json!({"max_bytes": max_bytes}))
    }
    pub fn rate_limited(retry_after_s: u64) -> ApiError {
        let mut e = ApiError::new(429, "rate_limited", "Too many requests.").details(json!({"retry_after_s": retry_after_s}));
        e.retry_after_s = Some(retry_after_s);
        e
    }
    pub fn internal(m: &str) -> ApiError {
        ApiError::new(500, "internal", m)
    }

    pub fn response(&self) -> Response {
        let r = Response::json(
            self.status,
            &json!({"error": {"code": self.code, "message": self.message, "details": self.details}}),
        );
        match self.retry_after_s {
            Some(s) => r.with_header("retry-after", &s.to_string()),
            None => r,
        }
    }
}

impl From<HostError> for ApiError {
    fn from(e: HostError) -> ApiError {
        ApiError::new(503, "unavailable", &format!("Storage error: {e}")).with_retry(5)
    }
}

impl ApiError {
    fn with_retry(mut self, s: u64) -> ApiError {
        self.retry_after_s = Some(s);
        self
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

// ---- pagination ----------------------------------------------------------------

pub const CURSOR_TTL_MS: i64 = 24 * 3_600_000;

/// `?limit=` (default 20, at most 100) and the offset from `?cursor=`.
pub fn page(req: &Request, now_ms: i64) -> ApiResult<(usize, usize)> {
    let limit = match req.q("limit") {
        None => 20,
        Some(s) => match s.parse::<usize>() {
            Ok(n) if (1..=100).contains(&n) => n,
            _ => return Err(ApiError::bad_request("limit must be 1..100").details(json!({"limit": s}))),
        },
    };
    let offset = match req.q("cursor") {
        None | Some("") => 0,
        Some(c) => decode_cursor(c, now_ms)?,
    };
    Ok((limit, offset))
}

/// An opaque cursor: base64url of "o<offset>.t<issued seconds>", well under
/// 64 characters (§5).
pub fn encode_cursor(offset: usize, now_ms: i64) -> String {
    util::b64url(format!("o{offset}.t{}", now_ms / 1000).as_bytes())
}

fn decode_cursor(c: &str, now_ms: i64) -> ApiResult<usize> {
    let bad = || ApiError::bad_request("bad cursor").details(json!({"cursor": "invalid"}));
    let raw = util::b64url_decode(c).ok_or_else(bad)?;
    let s = String::from_utf8(raw).map_err(|_| bad())?;
    let (o, t) = s.split_once('.').ok_or_else(bad)?;
    let offset: usize = o.strip_prefix('o').and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let issued: i64 = t.strip_prefix('t').and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    if now_ms - issued * 1000 > CURSOR_TTL_MS {
        return Err(ApiError::bad_request("the cursor expired").details(json!({"cursor": "expired"})));
    }
    Ok(offset)
}

/// `{"items", "next_cursor", "total"?}`.
pub fn list(items: Vec<Value>, offset: usize, limit: usize, total: Option<usize>, more: bool, now_ms: i64) -> Value {
    let next = if more { Value::String(encode_cursor(offset + limit, now_ms)) } else { Value::Null };
    let mut v = json!({"items": items, "next_cursor": next});
    if let Some(t) = total {
        v["total"] = t.into();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_expire_and_stay_short() {
        let now = 1_792_000_000_000;
        let c = encode_cursor(123_456, now);
        assert!(c.len() <= 64);
        let req = Request::new("GET", &format!("/x?cursor={c}&limit=5"));
        assert_eq!(page(&req, now).unwrap(), (5, 123_456));
        let late = page(&req, now + CURSOR_TTL_MS + 1000).unwrap_err();
        assert_eq!(late.details["cursor"], "expired");
        assert!(page(&Request::new("GET", "/x?limit=101"), now).is_err());
        assert!(page(&Request::new("GET", "/x?cursor=@@"), now).is_err());
    }

    #[test]
    fn query_decoding() {
        let r = Request::new("GET", "/a?x=1%2C2&y=a+b&z");
        assert_eq!(r.q("x"), Some("1,2"));
        assert_eq!(r.q("y"), Some("a b"));
        assert_eq!(r.q("z"), Some(""));
        assert_eq!(percent_decode("%4"), "%4");
    }
}
