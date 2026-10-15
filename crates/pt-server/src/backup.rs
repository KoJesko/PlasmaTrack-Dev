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
            // One statement per line: line breaks are spliced in with char().
            let mut out = String::from("'");
            for c in s.chars() {
                match c {
                    '\'' => out.push_str("''"),
                    '\n' => out.push_str("' || char(10) || '"),
                    '\r' => out.push_str("' || char(13) || '"),
                    c => out.push(c),
                }
            }
            out.push('\'');
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

    #[test]
    fn literals_keep_one_statement_per_line() {
        assert_eq!(literal(&json!(null)), "NULL");
        assert_eq!(literal(&json!(42)), "42");
        assert_eq!(literal(&json!(7.5)), "7.5");
        assert_eq!(literal(&json!("it's")), "'it''s'");
        assert_eq!(literal(&json!("a\nb\r\nc")), "'a' || char(10) || 'b' || char(13) || '' || char(10) || 'c'");
        assert_eq!(ident("we\"ird"), "\"we\"\"ird\"");
    }
}
