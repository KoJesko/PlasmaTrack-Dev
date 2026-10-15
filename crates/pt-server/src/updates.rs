//! Client updates and downloads (docs/UPDATES.md). A release published with a
//! signed update manifest is offered to clients; its files and the setup
//! program are in the release bucket. The server checks the signature when the
//! release is published but cannot sign anything itself: the key stays with the
//! maintainer, and clients verify again before installing.

use pt_core::update::{self, UpdateManifest};
use serde_json::{json, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::util::rfc3339;
use crate::{auth, params};

pub const MAX_NOTES_CHARS: usize = 2000;

/// What a release adds for updates, checked and ready to store.
pub struct UpdateCols {
    pub manifest: Option<String>,
    pub signature: Option<String>,
    pub installer: Option<String>,
    pub notes: Option<String>,
    pub offered: bool,
}

fn file_name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && !s.starts_with('.') && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

/// Checks the update part of `POST /admin/releases`: the signature, that the
/// manifest is for this version and platform and ships exactly the release's
/// plugin and client binaries, and that every file is in the release bucket.
pub async fn check<H: Host>(h: &H, body: &Value, version: &str, platform: &str, files: &[Value]) -> ApiResult<UpdateCols> {
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    let invalid = |field: &str, msg: String| ApiError::field(field, &msg);
    let notes = text("notes").map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if notes.as_ref().is_some_and(|n| n.chars().count() > MAX_NOTES_CHARS) {
        return Err(invalid("notes", format!("at most {MAX_NOTES_CHARS} characters")));
    }
    let installer = text("installer");
    if let Some(name) = &installer {
        if !file_name_ok(name) || !name.to_ascii_lowercase().ends_with(".exe") && platform == "windows" {
            return Err(invalid("installer", "a file name such as PlasmaTrack-Setup-0.2.0.exe".into()));
        }
        let key = format!("{platform}/{version}/{name}");
        if h.release_size(&key).await?.is_none() {
            return Err(invalid("installer", format!("{key} is not in the release bucket")));
        }
    }
    let (manifest, signature) = match body.get("update") {
        None | Some(Value::Null) => (None, None),
        Some(u) => {
            let m = u["manifest"].as_str().unwrap_or_default();
            let s = u["signature"].as_str().unwrap_or_default();
            let keys = match h.settings().update_keys.as_slice() {
                [] => update::UPDATE_KEYS,
                k => k,
            };
            let parsed = update::verify_with(m, s, keys).map_err(|e| invalid("update", e.to_string()))?;
            check_manifest(h, &parsed, version, platform, files).await?;
            (Some(m.to_string()), Some(s.trim().to_ascii_lowercase()))
        }
    };
    let offered = body.get("offer").and_then(Value::as_bool).unwrap_or(manifest.is_some());
    if offered && manifest.is_none() {
        return Err(invalid("offer", "only a release with an update can be offered".into()));
    }
    Ok(UpdateCols { manifest, signature, installer, notes, offered })
}

async fn check_manifest<H: Host>(h: &H, m: &UpdateManifest, version: &str, platform: &str, files: &[Value]) -> ApiResult<()> {
    let bad = |msg: String| ApiError::field("update", &msg);
    if m.version != version || m.platform != platform {
        return Err(bad(format!("the manifest is for {} {}", m.platform, m.version)));
    }
    let hashes: Vec<&str> = files.iter().filter_map(|f| f["sha256"].as_str()).collect();
    for f in files {
        let sha = f["sha256"].as_str().unwrap_or_default();
        if f["role"] != "config_file" && !m.files.iter().any(|u| u.sha256 == sha) {
            return Err(bad(format!("no file in the update has the {} hash {sha}", f["role"].as_str().unwrap_or_default())));
        }
    }
    for f in &m.files {
        let program = [".exe", ".dll", ".so"].iter().any(|x| f.path.to_ascii_lowercase().ends_with(x)) || !f.path.contains('.');
        if program && !hashes.contains(&f.sha256.as_str()) {
            return Err(bad(format!("{} is not one of the release's files", f.path)));
        }
        let key = m.key(f);
        match h.release_size(&key).await? {
            Some(n) if n == f.gz_size => {}
            Some(n) => return Err(bad(format!("{key} has {n} bytes, the manifest says {}", f.gz_size))),
            None => return Err(bad(format!("{key} is not in the release bucket"))),
        }
    }
    Ok(())
}

/// The update fields of a release in admin responses.
pub fn release_extras(r: &Row) -> Value {
    let manifest = r.opt_s("update_manifest").and_then(|m| update::parse(&m).ok());
    json!({
        "update": manifest.map(|m| json!({"files": m.files.len(), "bytes": m.files.iter().map(|f| f.gz_size).sum::<u64>()})),
        "installer": r.opt_s("installer"),
        "notes": r.opt_s("notes"),
        "offered": r.b("offered"),
    })
}

/// The newest offered release for a platform that nothing revokes (a scheduled
/// revocation counts: a release on its way out is not handed out).
async fn newest<H: Host>(h: &H, platform: &str) -> ApiResult<Option<Row>> {
    let rows = h
        .all(
            "SELECT * FROM releases WHERE platform = ?1 AND offered = 1 AND update_manifest IS NOT NULL
             AND release_id NOT IN (SELECT target_id FROM revocations WHERE target_kind = 'release' AND cancelled_at IS NULL)",
            params![platform],
        )
        .await?;
    Ok(rows.into_iter().max_by_key(|r| update::parse_version(&r.s("release_version"))))
}

fn platform_of(req: &Request) -> ApiResult<String> {
    let p = req.q("platform").unwrap_or("windows").to_string();
    if !matches!(p.as_str(), "windows" | "linux") {
        return Err(ApiError::bad_request("platform is windows or linux").details(json!({"platform": p})));
    }
    Ok(p)
}

/// `GET /api/v1/client/update?platform=windows&version=0.2.0`, asked by clients
/// without a token: the newest offered release when it is newer than `version`.
pub async fn client_update<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    let platform = platform_of(req)?;
    let current = req.q("version").and_then(update::parse_version).ok_or_else(|| ApiError::bad_request("version is like 0.2.0"))?;
    let offer = newest(h, &platform).await?.filter(|r| update::parse_version(&r.s("release_version")) > Some(current));
    let body = match offer {
        Some(r) => json!({"update": {
            "release_id": r.s("release_id"),
            "version": r.s("release_version"),
            "manifest": r.s("update_manifest"),
            "signature": r.s("update_signature"),
            "notes": r.opt_s("notes"),
            "published_at": rfc3339(r.i("created_at")),
        }}),
        None => json!({"update": null}),
    };
    Ok(Response::ok(body).with_header("cache-control", "no-store"))
}

fn setup_url<H: Host>(h: &H, platform: &str) -> String {
    format!("{}/dl/setup/{platform}", h.settings().public_url.trim_end_matches('/'))
}

/// `GET /api/v1/public/client?platform=windows`: what the bot links to.
pub async fn latest<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::service(h, req)?;
    let platform = platform_of(req)?;
    let r = newest(h, &platform).await?.ok_or_else(|| ApiError::not_found("release"))?;
    let installer = match r.opt_s("installer") {
        Some(name) => {
            let size = h.release_size(&format!("{platform}/{}/{name}", r.s("release_version"))).await?;
            json!({"file_name": name, "bytes": size, "url": setup_url(h, &platform)})
        }
        None => Value::Null,
    };
    Ok(Response::ok(json!({
        "platform": platform,
        "release_id": r.s("release_id"),
        "version": r.s("release_version"),
        "published_at": rfc3339(r.i("created_at")),
        "notes": r.opt_s("notes"),
        "installer": installer,
    })))
}

/// `GET /dl/setup/{platform}`: a redirect to the newest offered setup program,
/// so one link stays right across releases.
pub async fn setup<H: Host>(h: &H, platform: &str) -> ApiResult<Response> {
    let r = newest(h, platform).await?.ok_or_else(|| ApiError::not_found("release"))?;
    let name = r.opt_s("installer").ok_or_else(|| ApiError::not_found("release"))?;
    let to = format!("{}/dl/releases/{platform}/{}/{name}", h.settings().public_url.trim_end_matches('/'), r.s("release_version"));
    Ok(Response::bytes(302, "text/plain", Vec::new()).with_header("location", &to).with_header("cache-control", "no-store"))
}

/// The R2 key of a `/dl/releases/…` path, when it is one.
pub fn release_key(parts: &[&str]) -> Option<String> {
    let ok = parts.len() >= 3
        && matches!(parts[0], "windows" | "linux")
        && update::parse_version(parts[1]).is_some()
        && parts[2..].iter().all(|p| file_name_ok(p) || (!p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))));
    ok.then(|| parts.join("/"))
}

pub fn content_type(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or_default() {
        "gz" => "application/gzip",
        "exe" => "application/vnd.microsoft.portable-executable",
        _ => "application/octet-stream",
    }
}

/// Headers of a release file. Versions never change, so it can be cached.
pub fn file_headers(key: &str) -> Vec<(String, String)> {
    let mut v = vec![
        ("content-type".to_string(), content_type(key).to_string()),
        ("cache-control".to_string(), "public, max-age=31536000, immutable".to_string()),
    ];
    if !key.ends_with(".gz") {
        let name = key.rsplit('/').next().unwrap_or_default();
        v.push(("content-disposition".to_string(), format!("attachment; filename=\"{name}\"")));
    }
    v
}

/// `GET /dl/releases/{platform}/{version}/{path}`. The Worker streams these
/// itself (setup programs are large); this serves them in the native tests.
pub async fn file<H: Host>(h: &H, parts: &[&str]) -> ApiResult<Response> {
    let key = release_key(parts).ok_or_else(|| ApiError::not_found("file"))?;
    let bytes = h.release_get(&key).await?.ok_or_else(|| ApiError::not_found("file"))?;
    let mut r = Response::bytes(200, content_type(&key), bytes);
    r.headers = file_headers(&key);
    Ok(r)
}
