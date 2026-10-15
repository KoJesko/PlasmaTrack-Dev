//! Client updates (docs/UPDATES.md): a release published with a signed update
//! manifest is offered to older clients, its files and setup program are served
//! from the release bucket, and the bot gets one download link.

mod common;

use common::{SignedUpdate, TestHost};
use ed25519_dalek::Signer;
use pt_core::update::SIGNING_CONTEXT;
use serde_json::{json, Value};

fn gzip(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

fn build(t: &TestHost, version: &str) -> SignedUpdate {
    common::signed_update(t, version)
}

fn publish(t: &TestHost, version: &str, b: &SignedUpdate, extra: Value) -> pt_server::http::Response {
    let mut body = json!({
        "release_version": version, "platform": "windows", "files": b.files, "reason": format!("Release {version}"),
        "update": {"manifest": b.manifest, "signature": b.signature},
        "installer": format!("PlasmaTrack-Setup-{version}.exe"), "notes": "Faster uploads.",
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    t.advance(1000);
    t.call(t.admin("POST", "/api/v1/admin/releases").with_json(&body))
}

fn check(t: &TestHost, version: &str) -> Value {
    let r = t.call(pt_server::http::Request::new("GET", &format!("/api/v1/client/update?platform=windows&version={version}")));
    assert_eq!(r.status, 200, "{}", r.body_json());
    r.body_json()["update"].clone()
}

#[test]
fn a_signed_release_is_offered_to_older_clients() {
    let t = TestHost::new();
    let b = build(&t, "0.2.0");
    let r = publish(&t, "0.2.0", &b, json!({}));
    assert_eq!(r.status, 201, "{}", r.body_json());
    let rel = r.body_json();
    assert_eq!((rel["offered"].as_bool(), rel["update"]["files"].as_u64()), (Some(true), Some(4)));
    assert_eq!(rel["installer"], "PlasmaTrack-Setup-0.2.0.exe");

    let u = check(&t, "0.1.1");
    assert_eq!(u["version"], "0.2.0");
    assert_eq!(u["notes"], "Faster uploads.");
    let m = pt_core::update::verify_with(u["manifest"].as_str().unwrap(), u["signature"].as_str().unwrap(), &[common::update_signing_key().verifying_key().to_bytes()]).unwrap();
    assert_eq!(m.version, "0.2.0");
    assert_eq!(check(&t, "0.2.0"), Value::Null, "already current");
    assert_eq!(check(&t, "0.10.0"), Value::Null, "newer than anything offered");
    let bad = t.call(pt_server::http::Request::new("GET", "/api/v1/client/update?version=latest"));
    assert_eq!(bad.status, 400);

    // The files, compressed, straight from the bucket; cacheable.
    let f = t.call(pt_server::http::Request::new("GET", &format!("/dl/releases/{}", m.key(&m.files[2]))));
    assert_eq!(f.status, 200);
    assert_eq!(f.body, gzip(b"dll 0.2.0"));
    assert!(f.headers.iter().any(|(k, v)| k == "cache-control" && v.contains("immutable")));
    for p in ["/dl/releases/windows/0.2.0/../secret", "/dl/releases/windows/latest/x.gz", "/dl/releases/windows/0.2.0/missing.gz"] {
        assert_eq!(t.call(pt_server::http::Request::new("GET", p)).status, 404, "{p}");
    }

    // One stable setup link, and what the bot shows.
    let s = t.call(pt_server::http::Request::new("GET", "/dl/setup"));
    assert_eq!(s.status, 302);
    let to = s.headers.iter().find(|(k, _)| k == "location").unwrap().1.clone();
    assert_eq!(to, "https://pt.example/dl/releases/windows/0.2.0/PlasmaTrack-Setup-0.2.0.exe");
    let exe = t.call(pt_server::http::Request::new("GET", to.trim_start_matches("https://pt.example")));
    assert!(exe.headers.iter().any(|(k, v)| k == "content-disposition" && v.contains("PlasmaTrack-Setup-0.2.0.exe")));
    assert_eq!(t.call(pt_server::http::Request::new("GET", "/api/v1/public/client")).status, 401);
    let l = t.call(t.bot("GET", "/api/v1/public/client?platform=windows")).body_json();
    assert_eq!((l["version"].as_str(), l["installer"]["url"].as_str()), (Some("0.2.0"), Some("https://pt.example/dl/setup/windows")));
    assert_eq!(l["installer"]["bytes"], 5);
}

#[test]
fn the_newest_offered_release_wins_and_can_be_withdrawn() {
    let t = TestHost::new();
    let (a, b) = (build(&t, "0.2.0"), build(&t, "0.10.0"));
    assert_eq!(publish(&t, "0.10.0", &b, json!({})).status, 201);
    assert_eq!(publish(&t, "0.2.0", &a, json!({})).status, 201);
    assert_eq!(check(&t, "0.1.1")["version"], "0.10.0", "by version, not by date");

    // Withdrawn: the older one is offered again; offered again: back.
    let r = t.call(t.admin("POST", "/api/v1/admin/releases/rel_0_10_0_windows/offer").with_json(&json!({"offered": false, "reason": "Crashes on start"})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    assert_eq!(check(&t, "0.1.1")["version"], "0.2.0");
    t.advance(1000);
    t.call(t.admin("POST", "/api/v1/admin/releases/rel_0_10_0_windows/offer").with_json(&json!({"offered": true, "reason": "Fixed"})));
    assert_eq!(check(&t, "0.1.1")["version"], "0.10.0");

    // A scheduled revocation stops the offer at once.
    t.advance(1000);
    let r = t.call(t.admin("POST", "/api/v1/admin/revocations").with_json(&json!({"target": {"kind": "release", "id": "rel_0_10_0_windows"}, "reason": "Bad build"})));
    assert_eq!(r.status, 201, "{}", r.body_json());
    assert_eq!(check(&t, "0.1.1")["version"], "0.2.0");

    // A release without an update is never offered.
    t.advance(1000);
    let plain = json!({"release_version": "0.3.0", "platform": "windows", "files": a.files, "reason": "Hand-installed build"});
    assert_eq!(t.call(t.admin("POST", "/api/v1/admin/releases").with_json(&plain)).status, 201);
    assert_eq!(check(&t, "0.1.1")["version"], "0.2.0");
    let r = t.call(t.admin("POST", "/api/v1/admin/releases/rel_0_3_0_windows/offer").with_json(&json!({"offered": true, "reason": "Try"})));
    assert_eq!(r.status, 422);
}

#[test]
fn an_update_must_be_signed_complete_and_match_the_release() {
    let t = TestHost::new();
    let b = build(&t, "0.2.0");
    let refused = |extra: Value| {
        let r = publish(&t, "0.2.0", &b, extra);
        assert_eq!(r.status, 422, "{}", r.body_json());
        r.body_json()["error"]["details"].to_string()
    };
    // Signed by another key.
    let other = ed25519_dalek::SigningKey::from_bytes(&[1; 32]).sign(&[SIGNING_CONTEXT, b.manifest.as_bytes()].concat());
    let hex: String = other.to_bytes().iter().map(|x| format!("{x:02x}")).collect();
    assert!(refused(json!({"update": {"manifest": b.manifest, "signature": hex}})).contains("release key"));
    // Published under another version.
    assert!(publish(&t, "0.2.1", &b, json!({"installer": null})).body_json()["error"]["details"].to_string().contains("0.2.0"));
    // A program in the release that the update does not ship.
    let mut files = b.files.clone();
    files[0]["sha256"] = json!("00".repeat(32));
    assert!(refused(json!({"files": files})).contains("plugin hash"));
    // A file missing from the bucket, or of another size.
    let key = "windows/0.2.0/licenses/LLVM-LICENSE.txt.gz".to_string();
    let saved = t.releases.borrow_mut().remove(&key).unwrap();
    assert!(refused(json!({})).contains("not in the release bucket"));
    t.releases.borrow_mut().insert(key.clone(), b"short".to_vec());
    assert!(refused(json!({})).contains("bytes"));
    t.releases.borrow_mut().insert(key, saved);
    assert!(refused(json!({"installer": "Missing.exe"})).contains("installer"));
    assert!(refused(json!({"installer": "../x.exe"})).contains("installer"));
    assert!(refused(json!({"notes": "x".repeat(2001)})).contains("notes"));
    // All good.
    assert_eq!(publish(&t, "0.2.0", &b, json!({})).status, 201);
}
