//! Client updates and downloads (docs/UPDATES.md). A release published with a
//! signed update manifest is offered to clients; its files, the setup program
//! and the uninstaller are in the release bucket. Only the uninstaller is
//! public: updates need a player token, the setup a personal link the bot asks
//! for (owner, 2026-10-15: only some Discord roles may download the tracker). The server checks the signature when the
//! release is published but cannot sign anything itself: the key stays with the
//! maintainer, and clients verify again before installing.

use pt_core::update::{self, UpdateManifest};
use serde_json::{json, Value};

use crate::env::{Host, Row, RowExt};
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::store;
use crate::util::{self, rfc3339};
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

/// Offered releases for a platform that nothing revokes (a scheduled
/// revocation counts: a release on its way out is not handed out), newest first.
async fn offered<H: Host>(h: &H, platform: &str) -> ApiResult<Vec<Row>> {
    let mut rows = h
        .all(
            "SELECT * FROM releases WHERE platform = ?1 AND offered = 1 AND update_manifest IS NOT NULL
             AND release_id NOT IN (SELECT target_id FROM revocations WHERE target_kind = 'release' AND cancelled_at IS NULL)",
            params![platform],
        )
        .await?;
    rows.sort_by_key(|r| std::cmp::Reverse(update::parse_version(&r.s("release_version"))));
    Ok(rows)
}

async fn newest<H: Host>(h: &H, platform: &str) -> ApiResult<Option<Row>> {
    Ok(offered(h, platform).await?.into_iter().next())
}

fn platform_of(req: &Request) -> ApiResult<String> {
    let p = req.q("platform").unwrap_or("windows").to_string();
    if !matches!(p.as_str(), "windows" | "linux") {
        return Err(ApiError::bad_request("platform is windows or linux").details(json!({"platform": p})));
    }
    Ok(p)
}

/// `GET /api/v1/client/update?platform=windows&version=0.2.1`, asked by
/// clients with their player token: the newest offered release when it is
/// newer than `version`.
pub async fn client_update<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::player(h, req).await?;
    let platform = platform_of(req)?;
    let current = req.q("version").and_then(update::parse_version).ok_or_else(|| ApiError::bad_request("version is like 0.2.1"))?;
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

/// The uninstaller's name in every release folder. Anyone may download it.
pub const UNINSTALLER: &str = "PlasmaTrack-Uninstall.exe";
/// How long a personal setup link works.
pub const SETUP_LINK_TTL_MS: i64 = 3_600_000;
/// On a response from [`file`]: the release-bucket key whose bytes are the
/// body. The Worker streams the object and drops the header.
pub const STREAM_HEADER: &str = "x-plasmatrack-release-key";

fn origin<H: Host>(h: &H) -> String {
    h.settings().public_url.trim_end_matches('/').to_string()
}

fn release_sig<H: Host>(h: &H, key: &str, exp_s: i64) -> String {
    util::hex(&util::hmac_sha256(h.settings().download_secret.as_bytes(), format!("release.{key}.{exp_s}").as_bytes()))
}

/// The newest offered release whose folder holds the file `name` picks, with
/// that file's name and size.
async fn newest_with<H: Host>(h: &H, platform: &str, name: impl Fn(&Row) -> Option<String>) -> ApiResult<Option<(Row, String, u64)>> {
    for r in offered(h, platform).await? {
        if let Some(n) = name(&r) {
            if let Some(size) = h.release_size(&format!("{platform}/{}/{n}", r.s("release_version"))).await? {
                return Ok(Some((r, n, size)));
            }
        }
    }
    Ok(None)
}

/// `GET /api/v1/public/client?platform=windows`: the newest release, its
/// setup program (downloaded through a personal link, [`client_download`])
/// and the uninstaller (a public link).
pub async fn latest<H: Host>(h: &H, req: &Request) -> ApiResult<Response> {
    auth::service(h, req)?;
    let platform = platform_of(req)?;
    let r = newest(h, &platform).await?.ok_or_else(|| ApiError::not_found("release"))?;
    let installer = match r.opt_s("installer") {
        Some(name) => {
            let size = h.release_size(&format!("{platform}/{}/{name}", r.s("release_version"))).await?;
            json!({"file_name": name, "bytes": size})
        }
        None => Value::Null,
    };
    let uninstaller = newest_with(h, &platform, |_| Some(UNINSTALLER.to_string())).await?.map_or(Value::Null, |(_, name, size)| {
        json!({"file_name": name, "bytes": size, "url": format!("{}/dl/uninstall/{platform}", origin(h))})
    });
    Ok(Response::ok(json!({
        "platform": platform,
        "release_id": r.s("release_id"),
        "version": r.s("release_version"),
        "published_at": rfc3339(r.i("created_at")),
        "notes": r.opt_s("notes"),
        "installer": installer,
        "uninstaller": uninstaller,
    })))
}

/// `GET /api/v1/players/{discord_id}/client-download?platform=windows` (bot
/// token): a personal link to the newest setup program, valid for an hour.
/// The bot decides who may have one (Discord roles); a disabled player gets 403.
pub async fn client_download<H: Host>(h: &H, req: &Request, discord_id: &str) -> ApiResult<Response> {
    auth::bot(h, req)?;
    store::check_discord_id(discord_id)?;
    if store::user_by_discord(h, discord_id).await?.is_some_and(|u| u.disabled) {
        return Err(ApiError::forbidden("This player is disabled."));
    }
    let platform = platform_of(req)?;
    let (r, name, size) = newest_with(h, &platform, |r| r.opt_s("installer")).await?.ok_or_else(|| ApiError::not_found("release"))?;
    let key = format!("{platform}/{}/{name}", r.s("release_version"));
    let exp_s = (h.now_ms() + SETUP_LINK_TTL_MS) / 1000;
    let url = format!("{}/dl/releases/{key}?exp={exp_s}&sig={}", origin(h), release_sig(h, &key, exp_s));
    Ok(Response::ok(json!({
        "platform": platform,
        "release_id": r.s("release_id"),
        "version": r.s("release_version"),
        "notes": r.opt_s("notes"),
        "file_name": name,
        "bytes": size,
        "url": url,
        "expires_at": rfc3339(exp_s * 1000),
    }))
    .with_header("cache-control", "no-store"))
}

/// `GET /dl/uninstall/{platform}`: a redirect to the newest uninstaller, for
/// anyone, so one link stays right across releases.
pub async fn uninstall<H: Host>(h: &H, platform: &str) -> ApiResult<Response> {
    let (r, name, _) = newest_with(h, platform, |_| Some(UNINSTALLER.to_string())).await?.ok_or_else(|| ApiError::not_found("file"))?;
    let to = format!("{}/dl/releases/{platform}/{}/{name}", origin(h), r.s("release_version"));
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

/// `GET /dl/releases/{platform}/{version}/{path}`. Who may download what:
/// - update files (`.gz`): a client with a player token;
/// - the uninstaller: anyone;
/// - anything else (the setup program): a personal link from
///   [`client_download`].
///
/// The answer names the object in [`STREAM_HEADER`] with an empty body: the
/// Worker streams it (setup programs are large), and the native tests read
/// the header.
pub async fn file<H: Host>(h: &H, req: &Request, parts: &[&str]) -> ApiResult<Response> {
    let key = release_key(parts).ok_or_else(|| ApiError::not_found("file"))?;
    let name = key.rsplit('/').next().unwrap_or_default().to_string();
    let cache = if key.ends_with(".gz") {
        auth::player(h, req).await?;
        "private, max-age=86400"
    } else if name == UNINSTALLER {
        "public, max-age=31536000, immutable"
    } else {
        let bad = || ApiError::forbidden("This link is not valid. Ask the Discord bot for a new one.");
        let (Some(exp), Some(sig)) = (req.q("exp"), req.q("sig")) else { return Err(bad()) };
        let exp_s: i64 = exp.parse().map_err(|_| bad())?;
        if !util::constant_eq(sig.as_bytes(), release_sig(h, &key, exp_s).as_bytes()) {
            return Err(bad());
        }
        if h.now_ms() / 1000 > exp_s {
            return Err(ApiError::forbidden("This link has expired. Ask the Discord bot for a new one."));
        }
        "private, no-store"
    };
    h.release_size(&key).await?.ok_or_else(|| ApiError::not_found("file"))?;
    let mut r = Response::bytes(200, content_type(&key), Vec::new()).with_header("cache-control", cache).with_header(STREAM_HEADER, &key);
    if !key.ends_with(".gz") {
        r = r.with_header("content-disposition", &format!("attachment; filename=\"{name}\""));
    }
    Ok(r)
}
