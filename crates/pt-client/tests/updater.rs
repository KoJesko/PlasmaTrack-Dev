//! The updater against a fake server: it downloads only what a release key
//! signed, resumes, refuses tampered files, and swaps `bin` (and back).

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::{Signer, SigningKey};
use pt_client::updater;
use pt_core::update::{UpdateFile, UpdateManifest, PRODUCT, SIGNING_CONTEXT};
use sha2::{Digest, Sha256};

#[derive(Default)]
struct Fake {
    offer: Option<serde_json::Value>,
    files: BTreeMap<String, Vec<u8>>,
    downloads: usize,
}

type Shared = Arc<Mutex<Fake>>;

async fn offer(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"update": s.lock().unwrap().offer.clone()}))
}

async fn file(State(s): State<Shared>, UrlPath(key): UrlPath<String>) -> Result<Vec<u8>, StatusCode> {
    let mut s = s.lock().unwrap();
    s.downloads += 1;
    s.files.get(&key).cloned().ok_or(StatusCode::NOT_FOUND)
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[5; 32])
}

fn keys() -> Vec<[u8; 32]> {
    vec![key().verifying_key().to_bytes()]
}

fn gzip(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// A release with two files; returns the manifest text.
fn release(fake: &Shared, version: &str, signer: &SigningKey) -> String {
    let contents = [("plasmatrack-clientw.exe", format!("new client {version}")), ("licenses/LICENSE.txt", "licence".to_string())];
    let mut m = UpdateManifest { product: PRODUCT.into(), platform: updater::PLATFORM.into(), version: version.into(), files: vec![] };
    let mut f = fake.lock().unwrap();
    for (path, body) in contents {
        let gz = gzip(body.as_bytes());
        let uf = UpdateFile { path: path.into(), sha256: sha(body.as_bytes()), size: body.len() as u64, gz_size: gz.len() as u64 };
        f.files.insert(m.key(&uf), gz);
        m.files.push(uf);
    }
    let text = serde_json::to_string(&m).unwrap();
    let sig: String = signer.sign(&[SIGNING_CONTEXT, text.as_bytes()].concat()).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    f.offer = Some(serde_json::json!({"manifest": text, "signature": sig}));
    text
}

async fn serve(fake: Shared) -> String {
    let app = Router::new()
        .route("/api/v1/client/update", get(offer))
        .route("/dl/releases/{*key}", get(file))
        .with_state(fake);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

#[tokio::test]
async fn downloads_verifies_and_resumes() {
    let fake: Shared = Arc::default();
    let url = serve(fake.clone()).await;
    let http = reqwest::Client::new();
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join("updates");

    assert!(updater::fetch_offer(&http, &url, "0.1.0", &keys()).await.unwrap().is_none(), "nothing offered");
    release(&fake, "0.2.0", &key());
    let offer = updater::fetch_offer(&http, &url, "0.1.0", &keys()).await.unwrap().unwrap();
    assert!(updater::fetch_offer(&http, &url, "0.2.0", &keys()).await.is_err(), "not newer: refused");
    let staged = updater::stage(&http, &url, &offer, &updates).await.unwrap();
    assert_eq!(std::fs::read_to_string(staged.join("plasmatrack-clientw.exe")).unwrap(), "new client 0.2.0");
    assert_eq!(std::fs::read_to_string(staged.join("licenses").join("LICENSE.txt")).unwrap(), "licence");
    let (found, m) = updater::staged(&updates, "0.1.0", &keys()).unwrap();
    assert_eq!((found, m.version.as_str()), (staged.clone(), "0.2.0"));
    assert!(updater::staged(&updates, "0.2.0", &keys()).is_none(), "already installed");

    // Staging again downloads nothing; a damaged file is fetched again.
    let before = fake.lock().unwrap().downloads;
    updater::stage(&http, &url, &offer, &updates).await.unwrap();
    assert_eq!(fake.lock().unwrap().downloads, before);
    std::fs::write(staged.join("plasmatrack-clientw.exe"), "tampered").unwrap();
    assert!(updater::staged(&updates, "0.1.0", &keys()).is_none(), "a changed file is not installed");
    updater::stage(&http, &url, &offer, &updates).await.unwrap();
    assert_eq!(fake.lock().unwrap().downloads, before + 1);
    assert!(updater::staged(&updates, "0.1.0", &keys()).is_some());
}

#[tokio::test]
async fn refuses_unsigned_and_tampered_releases() {
    let fake: Shared = Arc::default();
    let url = serve(fake.clone()).await;
    let http = reqwest::Client::new();
    let dir = tempfile::tempdir().unwrap();

    release(&fake, "0.2.0", &SigningKey::from_bytes(&[6; 32]));
    let e = updater::fetch_offer(&http, &url, "0.1.0", &keys()).await.unwrap_err();
    assert!(e.contains("release key"), "{e}");

    // Signed, but the server hands out a different file.
    release(&fake, "0.2.0", &key());
    let offer = updater::fetch_offer(&http, &url, "0.1.0", &keys()).await.unwrap().unwrap();
    let k = offer.manifest.key(&offer.manifest.files[0]);
    let evil = gzip(b"evil client 0.2.0");
    let same_size = {
        let mut v = evil.clone();
        v.resize(offer.manifest.files[0].gz_size as usize, 0);
        v
    };
    fake.lock().unwrap().files.insert(k, same_size);
    let e = updater::stage(&http, &url, &offer, &dir.path().join("updates")).await.unwrap_err();
    assert!(e.contains("does not match") || e.contains("expected"), "{e}");
    assert!(updater::staged(&dir.path().join("updates"), "0.1.0", &keys()).is_none());
}

#[tokio::test]
async fn swaps_bin_and_restores_it() {
    let fake: Shared = Arc::default();
    let url = serve(fake.clone()).await;
    let http = reqwest::Client::new();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("plasmatrack-clientw.exe"), "old client").unwrap();
    std::fs::write(bin.join("old-only.txt"), "x").unwrap();

    release(&fake, "0.2.0", &key());
    let offer = updater::fetch_offer(&http, &url, "0.1.0", &keys()).await.unwrap().unwrap();
    let staged = updater::stage(&http, &url, &offer, &dir.path().join("updates")).await.unwrap();
    let m = updater::verify_staged(&staged, &keys()).unwrap();
    let old = updater::swap_bin(&staged, &m, &bin).unwrap();
    assert_eq!(std::fs::read_to_string(bin.join("plasmatrack-clientw.exe")).unwrap(), "new client 0.2.0");
    assert!(!bin.join("old-only.txt").exists(), "bin holds exactly the release");
    let listing: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(bin.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(listing["licenses/LICENSE.txt"], sha(b"licence"));
    assert_eq!(std::fs::read_to_string(old.join("plasmatrack-clientw.exe")).unwrap(), "old client");

    updater::restore_bin(&bin, &old).unwrap();
    assert_eq!(std::fs::read_to_string(bin.join("plasmatrack-clientw.exe")).unwrap(), "old client");
    assert!(bin.join("old-only.txt").exists());
}
