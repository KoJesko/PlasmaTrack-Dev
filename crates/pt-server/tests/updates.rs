//! Client updates (docs/UPDATES.md): a release published with a signed update
//! manifest is offered to older clients with a player token, the setup program
//! only through a personal link the bot asks for, and the uninstaller to anyone.

mod common;

use common::seed::{link, PLAYER};
use common::{SignedUpdate, TestHost};
use pt_server::http::{Request, Response};
use pt_server::updates::STREAM_HEADER;
use ed25519_dalek::Signer;
use pt_core::update::SIGNING_CONTEXT;
use serde_json::{json, Value};

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

/// A client with a player token.
fn client(t: &TestHost, path: &str) -> Response {
    let token = match t.q("SELECT 1 AS x FROM tokens").len() {
        0 => link(t),
        _ => TOKEN.with(|k| k.borrow().clone()),
    };
    TOKEN.with(|k| *k.borrow_mut() = token.clone());
    t.call(Request::new("GET", path).with_header("authorization", &format!("Bearer {token}")))
}

thread_local! {
    static TOKEN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn check(t: &TestHost, version: &str) -> Value {
    let r = client(t, &format!("/api/v1/client/update?platform=windows&version={version}"));
    assert_eq!(r.status, 200, "{}", r.body_json());
    r.body_json()["update"].clone()
}

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn anyone(t: &TestHost, path: &str) -> Response {
    t.call(Request::new("GET", path))
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
    assert_eq!(client(&t, "/api/v1/client/update?version=latest").status, 400);
    assert_eq!(anyone(&t, "/api/v1/client/update?platform=windows&version=0.1.1").status, 401, "a client needs its token");

    // Update files: for clients with a token, streamed by the Worker.
    let dll = format!("/dl/releases/{}", m.key(&m.files[2]));
    let f = client(&t, &dll);
    assert_eq!(f.status, 200);
    assert_eq!(header(&f, STREAM_HEADER), Some("windows/0.2.0/plasmatrack.dll.gz"));
    assert!(f.body.is_empty());
    assert_eq!(anyone(&t, &dll).status, 401);
    for p in ["/dl/releases/windows/0.2.0/../secret", "/dl/releases/windows/latest/x.gz", "/dl/releases/windows/0.2.0/missing.gz"] {
        assert_eq!(client(&t, p).status, 404, "{p}");
    }
    assert_eq!(anyone(&t, "/dl/setup").status, 404, "no public setup link");
}

#[test]
fn the_setup_needs_a_personal_link_and_the_uninstaller_is_public() {
    let t = TestHost::new();
    let b = build(&t, "0.2.0");
    assert_eq!(publish(&t, "0.2.0", &b, json!({})).status, 201);
    let setup = "/dl/releases/windows/0.2.0/PlasmaTrack-Setup-0.2.0.exe";
    assert_eq!(anyone(&t, setup).status, 403);
    assert_eq!(client(&t, setup).status, 403, "a token is not a link");

    // The bot asks for a link for a member with the right role.
    assert_eq!(t.call(Request::new("GET", "/api/v1/players/555/client-download")).status, 401);
    let r = t.call(t.bot("GET", "/api/v1/players/555/client-download?platform=windows"));
    assert_eq!(r.status, 200, "{}", r.body_json());
    let d = r.body_json();
    assert_eq!((d["version"].as_str(), d["file_name"].as_str(), d["bytes"].as_u64()), (Some("0.2.0"), Some("PlasmaTrack-Setup-0.2.0.exe"), Some(5)));
    assert!(t.q("SELECT 1 AS x FROM users WHERE discord_user_id = '555'").is_empty(), "nobody is created");
    let url = d["url"].as_str().unwrap().trim_start_matches("https://pt.example").to_string();
    let f = anyone(&t, &url);
    assert_eq!(f.status, 200, "{}", f.body_json());
    assert_eq!(header(&f, STREAM_HEADER), Some("windows/0.2.0/PlasmaTrack-Setup-0.2.0.exe"));
    assert!(header(&f, "content-disposition").unwrap().contains("PlasmaTrack-Setup-0.2.0.exe"));
    assert_eq!(header(&f, "cache-control"), Some("private, no-store"));
    // Another file, or a changed expiry, does not pass; an hour later it expires.
    assert_eq!(anyone(&t, &url.replace("PlasmaTrack-Setup-0.2.0.exe", "plasmatrack.dll")).status, 403);
    let exp: i64 = url.split("exp=").nth(1).unwrap().split('&').next().unwrap().parse().unwrap();
    assert_eq!(anyone(&t, &url.replace(&format!("exp={exp}"), &format!("exp={}", exp + 3600))).status, 403);
    t.advance(3_601_000);
    let late = anyone(&t, &url);
    assert_eq!(late.status, 403);
    assert!(late.body_json()["error"]["message"].as_str().unwrap().contains("expired"));

    // A disabled player gets none (linked above, by client()).
    t.advance(1000);
    let r = t.call(t.admin("PATCH", &format!("/api/v1/admin/players/{PLAYER}")).with_json(&json!({"disabled": true, "reason": "Cheating"})));
    assert_eq!(r.status, 200, "{}", r.body_json());
    assert_eq!(t.call(t.bot("GET", &format!("/api/v1/players/{PLAYER}/client-download"))).status, 403);

    // The uninstaller: one link for anyone, cacheable.
    let u = anyone(&t, "/dl/uninstall");
    assert_eq!(u.status, 302);
    let to = header(&u, "location").unwrap().trim_start_matches("https://pt.example").to_string();
    assert_eq!(to, "/dl/releases/windows/0.2.0/PlasmaTrack-Uninstall.exe");
    let f = anyone(&t, &to);
    assert_eq!((f.status, header(&f, STREAM_HEADER)), (200, Some("windows/0.2.0/PlasmaTrack-Uninstall.exe")));
    assert!(header(&f, "cache-control").unwrap().contains("public"));

    // What the bot shows: no setup link, the uninstaller's link.
    assert_eq!(anyone(&t, "/api/v1/public/client").status, 401);
    let l = t.call(t.bot("GET", "/api/v1/public/client?platform=windows")).body_json();
    assert_eq!(l["version"], "0.2.0");
    assert_eq!(l["installer"], json!({"file_name": "PlasmaTrack-Setup-0.2.0.exe", "bytes": 5}));
    assert_eq!(l["uninstaller"]["url"], "https://pt.example/dl/uninstall/windows");
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
