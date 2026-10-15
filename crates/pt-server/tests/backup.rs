//! Weekly backups (docs/SERVER.md): a dump restores into an empty database
//! exactly, and only the newest two are kept.

mod common;

use common::seed::*;
use common::{block_on, TestHost};
use serde_json::json;

const TABLES: [&str; 15] = [
    "users", "tokens", "plugin_sessions", "raw_chunks", "jobs", "truckersmp_accounts", "bonuses", "audit_log",
    "config_versions", "releases", "revocations", "webhook_outbox", "operations", "evidence", "idempotency",
];

fn table(t: &TestHost, name: &str) -> Vec<serde_json::Value> {
    t.q(&format!("SELECT * FROM {name} ORDER BY rowid"))
}

#[test]
fn a_backup_restores_every_table_exactly() {
    let t = TestHost::new();
    chicago(&t);
    manual_job(&t);
    // Text with quotes and line breaks survives.
    t.call(t.admin("POST", "/api/v1/admin/bonuses").with_json(&json!({"game": "ats", "label": "It's\nnew\r\nweek",
        "match": "x*", "match_kind": "glob", "kind": "flat", "amount": 1.5,
        "active_from": "2026-10-01T00:00:00Z", "active_to": "2026-11-01T00:00:00Z", "reason": "Line\nbreaks"})));
    let report = block_on(pt_server::backup::run(&t)).unwrap();
    assert_eq!(report.key, "d1/test-server/2026-10-14T09-00-00.000Z.sql");
    assert!(report.tables.iter().all(|(n, _)| n != "idempotency"));
    let sql = String::from_utf8(t.backups.borrow()[&report.key].clone()).unwrap();
    assert!(sql.lines().all(|l| l.is_empty() || l.starts_with("--") || l.ends_with(';')), "one statement per line");

    // Restore into a fresh database (migrations applied, as on D1).
    let fresh = TestHost::new();
    fresh.exec(&sql);
    for name in TABLES.iter().filter(|n| **n != "idempotency") {
        assert_eq!(table(&fresh, name), table(&t, name), "{name}");
    }
    assert_eq!(fresh.q("SELECT label FROM bonuses")[0]["label"], "It's\nnew\r\nweek");
    // Restoring twice is the same: each table is emptied first.
    fresh.exec(&sql);
    assert_eq!(table(&fresh, "jobs"), table(&t, "jobs"));
}

#[test]
fn only_the_newest_two_are_kept() {
    let t = TestHost::new();
    link(&t);
    let mut keys = Vec::new();
    for _ in 0..4 {
        keys.push(block_on(pt_server::backup::run(&t)).unwrap().key);
        t.advance(7 * 86_400_000);
    }
    let left: Vec<String> = t.backups.borrow().keys().cloned().collect();
    assert_eq!(left, keys[2..].to_vec());
    // Other prefixes (the bot's, the other server's) are never touched.
    t.backups.borrow_mut().insert("d1/plasmatrack-bot/2020-01-01T00-00-00.000Z.sql".into(), vec![]);
    block_on(pt_server::backup::run(&t)).unwrap();
    assert!(t.backups.borrow().contains_key("d1/plasmatrack-bot/2020-01-01T00-00-00.000Z.sql"));
    assert_eq!(t.backups.borrow().keys().filter(|k| k.starts_with("d1/test-server/")).count(), 2);

    // Moderators can take one now; it is audited.
    let r = t.call(t.admin("POST", "/api/v1/admin/backups").with_json(&json!({"reason": "Before a config change"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    assert!(r.body_json()["rows"]["users"].as_u64().unwrap() >= 1);
    assert_eq!(t.q("SELECT action FROM audit_log WHERE action = 'backup.create'").len(), 1);
}
