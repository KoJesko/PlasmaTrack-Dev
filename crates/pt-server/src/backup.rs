//! Weekly backups of the D1 database to the backup bucket (docs/SERVER.md),
//! the same format as the bot's: one SQL file, per table `DELETE FROM t;` then
//! one `INSERT` per row, one statement per line, restorable with
//! `wrangler d1 execute <db> --remote --file <file>`. The newest two are kept.

use serde_json::{json, Value};

use crate::env::{Host, RowExt};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::params;
use crate::store::{self, Audit};
use crate::util::rfc3339;

/// Backups kept per server.
pub const KEEP: usize = 2;
/// Rows read per query.
const PAGE: i64 = 500;
/// Tables not worth restoring: replay protection that expires within a day.
const SKIP: [&str; 1] = ["idempotency"];

/// One SQL literal.
fn literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => (*b as i32).to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            // One statement per line: line breaks become a marker the text does
            // not contain, turned back by replace(). D1 allows expressions only
            // 100 deep, so a `|| char(10) ||` per line breaks on long texts
            // (the published config's TOML).
            // A marker counts only if decoding gives the text back: "~n0" before
            // a line break would otherwise become "~n0~n0~", which replace()
            // matches at the wrong place. \r is encoded first, so it is decoded
            // last (the outer replace).
            let mut t = s.clone();
            let mut steps = Vec::new();
            for (ch, name, code) in [('\r', "r", 13), ('\n', "n", 10)] {
                if t.contains(ch) {
                    let back = ch.to_string();
                    let m = (0..)
                        .map(|k| format!("~{name}{k}~"))
                        .find(|m| t.replace(ch, m).replace(m.as_str(), &back) == t)
                        .expect("some marker round-trips");
                    t = t.replace(ch, &m);
                    steps.push((m, code));
                }
            }
            let mut out = format!("'{}'", t.replace('\'', "''"));
            for (m, code) in steps.iter().rev() {
                out = format!("replace({out}, '{m}', char({code}))");
            }
            out
        }
        other => literal(&Value::String(other.to_string())),
    }
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[derive(Debug, Default)]
pub struct Report {
    pub key: String,
    pub bytes: usize,
    pub tables: Vec<(String, usize)>,
    pub deleted: Vec<String>,
}

/// The SQL dump of every table, and the row count per table.
pub async fn dump<H: Host>(h: &H) -> ApiResult<(String, Vec<(String, usize)>)> {
    let tables = h
        .all(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'
                 AND name != 'd1_migrations' ORDER BY name",
            vec![],
        )
        .await?;
    let mut out = String::from("-- PlasmaTrack server backup. Restore: wrangler d1 execute <db> --remote --file <this file>\n");
    let mut counts = Vec::new();
    for t in tables.iter().map(|r| r.s("name")).filter(|n| !SKIP.contains(&n.as_str())) {
        out.push_str(&format!("DELETE FROM {};\n", ident(&t)));
        let (mut last, mut n) = (i64::MIN, 0usize);
        loop {
            let rows = h
                .all(&format!("SELECT rowid AS \"_backup_rowid\", * FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2", ident(&t)), params![last, PAGE])
                .await?;
            for r in &rows {
                last = r.i("_backup_rowid");
                let cols: Vec<&String> = r.keys().filter(|k| *k != "_backup_rowid").collect();
                let names: Vec<String> = cols.iter().map(|c| ident(c)).collect();
                let values: Vec<String> = cols.iter().map(|c| literal(&r[c.as_str()])).collect();
                out.push_str(&format!("INSERT INTO {} ({}) VALUES ({});\n", ident(&t), names.join(", "), values.join(", ")));
                n += 1;
            }
            if (rows.len() as i64) < PAGE {
                break;
            }
        }
        counts.push((t, n));
    }
    Ok((out, counts))
}

/// Writes a backup, then deletes all but the newest [`KEEP`]. A failed write
/// deletes nothing.
pub async fn run<H: Host>(h: &H) -> ApiResult<Report> {
    let prefix = h.settings().backup_prefix.clone();
    if prefix.is_empty() {
        return Err(ApiError::internal("no backup prefix configured"));
    }
    let (sql, tables) = dump(h).await?;
    let key = format!("{prefix}{}.sql", rfc3339(h.now_ms()).replace(':', "-"));
    let bytes = sql.len();
    h.backup_put(&key, sql.into_bytes()).await?;
    let mut keys: Vec<String> = h.backup_list(&prefix).await?.into_iter().filter(|k| k.ends_with(".sql")).collect();
    keys.sort();
    keys.reverse();
    let mut deleted = Vec::new();
    for k in keys.into_iter().skip(KEEP).filter(|k| *k != key) {
        h.backup_delete(&k).await?;
        deleted.push(k);
    }
    Ok(Report { key, bytes, tables, deleted })
}

pub fn report_json(r: &Report) -> Value {
    let tables: serde_json::Map<String, Value> = r.tables.iter().map(|(t, n)| (t.clone(), json!(n))).collect();
    json!({"key": r.key, "size_bytes": r.bytes, "rows": tables, "deleted": r.deleted})
}

/// `POST /api/v1/admin/backups`: a backup now, for example before a risky change.
pub async fn now<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let admin = crate::auth::admin(h, req).await?;
    let idem = match store::idem_begin(h, req, "bot").await? {
        Ok(i) => i,
        Err(replay) => return Ok(replay),
    };
    let reason = store::reason(&req.json_body()?)?;
    let report = run(h).await?;
    let v = report_json(&report);
    store::audit(h, Audit {
        actor: &admin.discord_user_id, action: "backup.create", target_kind: "backup", target_id: &report.key,
        target_user: None, reason: Some(&reason), before: json!({}), after: json!({"size_bytes": report.bytes, "deleted": report.deleted}),
    })
    .await?;
    store::idem_store(h, idem, 201, &v).await?;
    Ok(Response::json(201, &v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluates a text literal as SQLite would: nested replace(…, 'm', char(c))
    /// around one quoted string (left to right, non-overlapping, like
    /// `str::replace`).
    fn decode(lit: &str) -> String {
        let mut l = lit;
        let mut calls = Vec::new();
        while let Some(inner) = l.strip_prefix("replace(") {
            let close = inner.rfind(", '").unwrap();
            let (rest, tail) = (&inner[..close], &inner[close + 3..]);
            let (m, code) = tail.split_once("', char(").unwrap();
            calls.push((m.to_string(), code.trim_end_matches(')').parse::<u8>().unwrap() as char));
            l = rest;
        }
        let mut v = l.strip_prefix('\'').unwrap().strip_suffix('\'').unwrap().replace("''", "'");
        for (m, c) in calls.iter().rev() {
            v = v.replace(m.as_str(), &c.to_string());
        }
        v
    }

    #[test]
    fn literals_keep_one_statement_per_line() {
        assert_eq!(literal(&json!(null)), "NULL");
        assert_eq!(literal(&json!(42)), "42");
        assert_eq!(literal(&json!(7.5)), "7.5");
        assert_eq!(literal(&json!("it's")), "'it''s'");
        assert_eq!(literal(&json!("a\nb\r\nc")), "replace(replace('a~n0~b~r0~~n0~c', '~n0~', char(10)), '~r0~', char(13))");
        // A marker already in the text is skipped, and so is one a line break
        // would complete (found by the bot's session).
        assert_eq!(literal(&json!("~n0~\n")), "replace('~n0~~n1~', '~n1~', char(10))");
        assert_eq!(literal(&json!("~n0\n")), "replace('~n0~n1~', '~n1~', char(10))");
        // Decoding the way SQLite's replace() does gives every text back.
        for s in ["~n0\n", "\n~n0~", "~r0\r\n", "~n0~r0\r~n1\n", "~\n~", "~n\n0~", "a\r\r\n\n'b'~r0~~n0~", &"~n0\n".repeat(50)] {
            assert_eq!(decode(&literal(&json!(s))), s, "{s:?}");
        }
        // Depth stays constant however many lines (D1 allows 100).
        let long = literal(&json!("x\n".repeat(500)));
        assert_eq!((long.matches("replace(").count(), long.contains('\n')), (1, false));
        assert_eq!(ident("we\"ird"), "\"we\"\"ird\"");
    }
}
