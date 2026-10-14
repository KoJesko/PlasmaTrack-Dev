//! Signed webhooks to the bot (docs/WEBHOOKS.md). Events go to an outbox in
//! D1; the queue delivers them with the retry schedule of §3 and drops them
//! after 72 h.

use serde_json::{json, Value};

use crate::env::{Host, RowExt};
use crate::http::ApiResult;
use crate::params;
use crate::store;
use crate::util;

/// Seconds to wait after attempt n fails (§3), then every 8 h.
const RETRY_S: [u32; 7] = [30, 120, 600, 1800, 3600, 7200, 14_400];
const RETRY_LATER_S: u32 = 28_800;
pub const MAX_AGE_MS: i64 = 72 * 3_600_000;
pub const MAX_BODY_BYTES: usize = 256 << 10;
pub const TIMESTAMP_TOLERANCE_S: i64 = 300;

/// Records an event and queues its delivery after `delay_s` seconds.
pub async fn emit<H: Host>(h: &H, kind: &str, data: Value, delay_s: u32) -> ApiResult<String> {
    let event_id = store::new_id(h, "evt");
    let now = h.now_ms();
    let body = json!({
        "event_id": event_id,
        "type": kind,
        "occurred_at": util::rfc3339(now),
        "payload_version": 1,
        "data": data,
    });
    h.run(
        "INSERT INTO webhook_outbox (event_id, type, body, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&event_id, kind, body.to_string(), now],
    )
    .await?;
    h.queue_webhook(&event_id, delay_s).await?;
    Ok(event_id)
}

/// `t=<unix s>,v1=<hex>[,v1=<hex>…]`: one `v1` per configured secret.
pub fn signature(secrets: &[String], t: i64, body: &[u8]) -> String {
    let mut msg = format!("{t}.").into_bytes();
    msg.extend_from_slice(body);
    let mut s = format!("t={t}");
    for secret in secrets {
        s.push_str(&format!(",v1={}", util::hex(&util::hmac_sha256(secret.as_bytes(), &msg))));
    }
    s
}

/// Checks a signature header the way the bot does (§2); used by the tests.
pub fn verify(secret: &str, header: &str, body: &[u8], now_s: i64) -> bool {
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_string()),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if (now_s - t).abs() > TIMESTAMP_TOLERANCE_S {
        return false;
    }
    let expected = signature(&[secret.to_string()], t, body);
    let want = expected.rsplit_once("v1=").map(|(_, v)| v).unwrap_or_default();
    sigs.iter().any(|s| util::constant_eq(s.as_bytes(), want.as_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Done,
    /// Try again after this many seconds.
    RetryIn(u32),
    Dropped,
}

/// One delivery attempt of an outbox event.
pub async fn deliver<H: Host>(h: &H, event_id: &str) -> ApiResult<Delivery> {
    let Some(row) = store::one(h, "SELECT * FROM webhook_outbox WHERE event_id = ?1", params![event_id]).await? else {
        return Ok(Delivery::Dropped);
    };
    if row.opt_i("delivered_at").is_some() {
        return Ok(Delivery::Done);
    }
    if row.opt_i("dropped_at").is_some() {
        return Ok(Delivery::Dropped);
    }
    let now = h.now_ms();
    if now - row.i("created_at") > MAX_AGE_MS {
        h.run("UPDATE webhook_outbox SET dropped_at = ?2 WHERE event_id = ?1", params![event_id, now]).await?;
        return Ok(Delivery::Dropped);
    }
    let attempts = row.i("attempts");
    let retry = RETRY_S.get(attempts as usize).copied().unwrap_or(RETRY_LATER_S);
    let settings = h.settings();
    let (Some(url), false) = (settings.webhook_url.clone(), settings.webhook_secrets.is_empty()) else {
        h.run(
            "UPDATE webhook_outbox SET attempts = attempts + 1, last_error = 'no webhook URL or secret configured' WHERE event_id = ?1",
            params![event_id],
        )
        .await?;
        return Ok(Delivery::RetryIn(retry));
    };
    let body = row.s("body").into_bytes();
    let headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        ("x-plasmatrack-event".to_string(), row.s("type")),
        ("x-plasmatrack-event-id".to_string(), event_id.to_string()),
        ("x-plasmatrack-signature".to_string(), signature(&settings.webhook_secrets, now / 1000, &body)),
    ];
    let outcome = h.http_post(&url, headers, body).await;
    match outcome {
        Ok(status) if (200..300).contains(&status) => {
            h.run("UPDATE webhook_outbox SET attempts = attempts + 1, delivered_at = ?2 WHERE event_id = ?1", params![event_id, now])
                .await?;
            Ok(Delivery::Done)
        }
        other => {
            let error = match other {
                Ok(status) => format!("HTTP {status}"),
                Err(e) => e.0,
            };
            h.run(
                "UPDATE webhook_outbox SET attempts = attempts + 1, last_error = ?2 WHERE event_id = ?1",
                params![event_id, error],
            )
            .await?;
            Ok(Delivery::RetryIn(retry))
        }
    }
}
