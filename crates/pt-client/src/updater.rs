//! Automatic updates (docs/UPDATES.md).
//!
//! The windowless client asks the server for a newer release every few hours.
//! It downloads the release's files into `updates/<version>` in the data
//! directory, checking each against the update manifest, which must carry a
//! signature by a release key built into this client (`pt_core::update`).
//! Once no game is running, it starts `plasmatrack-clientw apply-update` from
//! that folder and quits. The new program then swaps the `bin` folder,
//! carries the player's `[local]` settings into the config it ships, copies the
//! plugin into every game that has it, and starts the new client. If the new
//! client does not come up, the old `bin` folder is put back.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pt_core::update::{self, UpdateManifest, DOWNLOAD_PATH, UPDATE_PATH};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{log, setup, steam};

pub const UPDATES_DIR: &str = "updates";
/// The update manifest and its signature, kept next to the staged files.
pub const STAGED_MANIFEST: &str = "update.json";
pub const STAGED_SIGNATURE: &str = "update.sig";
/// Written by `apply-update` for the new client to announce, then removed.
pub const UPDATED_FILE: &str = "updated.json";
/// Written when an update could not be installed; that version is not tried
/// again for [`RETRY_FAILED`].
pub const FAILED_FILE: &str = "update-failed.json";
pub const RETRY_FAILED: Duration = Duration::from_secs(6 * 3600);
pub const GAME_PROCESSES: [&str; 2] = ["eurotrucks2.exe", "amtrucks.exe"];
pub const PLATFORM: &str = if cfg!(windows) { "windows" } else { "linux" };

/// An update the server offers, with its signature checked.
#[derive(Debug, Clone)]
pub struct Offer {
    pub manifest: UpdateManifest,
    pub text: String,
    pub signature: String,
}

#[derive(Deserialize)]
struct OfferBody {
    update: Option<OfferJson>,
}

#[derive(Deserialize)]
struct OfferJson {
    manifest: String,
    signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub version: String,
    pub from: String,
    pub at_ms: i64,
    pub error: Option<String>,
}

pub fn newer(version: &str, than: &str) -> bool {
    match (update::parse_version(version), update::parse_version(than)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Asks the server whether there is something newer than `current`.
pub async fn fetch_offer(http: &reqwest::Client, server_url: &str, current: &str, keys: &[[u8; 32]]) -> Result<Option<Offer>, String> {
    let url = format!("{}{UPDATE_PATH}?platform={PLATFORM}&version={current}", server_url.trim_end_matches('/'));
    let res = http.get(url).send().await.map_err(|e| format!("update check: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("update check: server returned {}", res.status()));
    }
    let text = res.text().await.map_err(|e| format!("update check: {e}"))?;
    let body: OfferBody = serde_json::from_str(&text).map_err(|e| format!("update check: {e}"))?;
    let Some(o) = body.update else { return Ok(None) };
    let manifest = update::verify_with(&o.manifest, &o.signature, keys).map_err(|e| format!("update refused: {e}"))?;
    if manifest.platform != PLATFORM || !newer(&manifest.version, current) {
        return Err(format!("update refused: {} {} is not newer for {PLATFORM}", manifest.platform, manifest.version));
    }
    Ok(Some(Offer { manifest, text: o.manifest, signature: o.signature }))
}

fn file_matches(path: &Path, f: &update::UpdateFile) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == f.size) && crate::manifest::sha256_file(path).is_ok_and(|h| h == f.sha256)
}

/// Downloads every file of the offer into `updates_dir/<version>`; files
/// already there with the right hash are kept, so an interrupted download
/// resumes. The manifest and signature are written last: their presence means
/// the folder is complete.
pub async fn stage(http: &reqwest::Client, server_url: &str, offer: &Offer, updates_dir: &Path) -> Result<PathBuf, String> {
    let m = &offer.manifest;
    let dir = updates_dir.join(&m.version);
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    let _ = std::fs::remove_file(dir.join(STAGED_MANIFEST));
    for f in &m.files {
        let dest = f.local_path(&dir);
        if file_matches(&dest, f) {
            continue;
        }
        let url = format!("{}{DOWNLOAD_PATH}{}", server_url.trim_end_matches('/'), m.key(f));
        let res = http.get(&url).send().await.map_err(|e| format!("{url}: {e}"))?;
        if !res.status().is_success() {
            return Err(format!("{url}: server returned {}", res.status()));
        }
        if res.content_length().is_some_and(|n| n != f.gz_size) {
            return Err(format!("{url}: unexpected size"));
        }
        let gz = res.bytes().await.map_err(|e| format!("{url}: {e}"))?;
        if gz.len() as u64 != f.gz_size {
            return Err(format!("{url}: {} bytes, expected {}", gz.len(), f.gz_size));
        }
        let mut data = Vec::with_capacity(f.size as usize);
        flate2::read::GzDecoder::new(&gz[..]).take(f.size + 1).read_to_end(&mut data).map_err(|e| format!("{url}: {e}"))?;
        if data.len() as u64 != f.size || hex_sha256(&data) != f.sha256 {
            return Err(format!("{}: does not match the signed manifest", f.path));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let part = dest.with_extension("part");
        std::fs::write(&part, &data).map_err(io)?;
        std::fs::rename(&part, &dest).map_err(io)?;
    }
    std::fs::write(dir.join(STAGED_SIGNATURE), &offer.signature).map_err(io)?;
    std::fs::write(dir.join(STAGED_MANIFEST), &offer.text).map_err(io)?;
    Ok(dir)
}

/// A complete staged update in `dir`: signature good, every file present and
/// unchanged.
pub fn verify_staged(dir: &Path, keys: &[[u8; 32]]) -> Result<UpdateManifest, String> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()));
    let m = update::verify_with(&read(STAGED_MANIFEST)?, &read(STAGED_SIGNATURE)?, keys).map_err(|e| e.to_string())?;
    for f in &m.files {
        if !file_matches(&f.local_path(dir), f) {
            return Err(format!("{} is missing or changed", f.path));
        }
    }
    Ok(m)
}

/// The newest complete staged update newer than `current`.
pub fn staged(updates_dir: &Path, current: &str, keys: &[[u8; 32]]) -> Option<(PathBuf, UpdateManifest)> {
    let mut found: Vec<(PathBuf, UpdateManifest)> = std::fs::read_dir(updates_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(STAGED_MANIFEST).is_file())
        .filter_map(|p| verify_staged(&p, keys).ok().map(|m| (p, m)))
        .filter(|(_, m)| m.platform == PLATFORM && newer(&m.version, current))
        .collect();
    found.sort_by_key(|(_, m)| update::parse_version(&m.version));
    found.pop()
}

/// Removes staged updates that are not newer than `current` (installed, or
/// superseded).
pub fn clean(updates_dir: &Path, current: &str) {
    for e in std::fs::read_dir(updates_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if update::parse_version(&name).is_some() && !newer(&name, current) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// True while ETS2 or ATS runs (their plugin DLL cannot be replaced then).
/// When the process list cannot be read, it says yes, to be safe.
#[cfg(windows)]
pub fn games_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    // SAFETY: a process snapshot is walked with a correctly sized entry and closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return true;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more && !found {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_ascii_lowercase();
            found = GAME_PROCESSES.contains(&name.as_str());
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(windows))]
pub fn games_running() -> bool {
    true
}

/// The client's `bin` folder when this program runs from the installed one
/// (`%LOCALAPPDATA%\PlasmaTrack\bin`). Only then does it update itself.
pub fn installed_bin(data_dir: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?.to_path_buf();
    let expected = data_dir.join("bin");
    let same = match (bin.canonicalize(), expected.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    (same && cfg!(windows)).then_some(bin)
}

/// Builds the new `bin` from the staged files (and the `manifest.json` the
/// installer would write), then swaps it in. Returns where the old one went.
/// Renaming `bin` fails while any program in it runs, so this retries for a
/// while after the old client quit.
pub fn swap_bin(staged: &Path, m: &UpdateManifest, bin: &Path) -> Result<PathBuf, String> {
    let parent = bin.parent().ok_or("bin has no parent folder")?;
    let new = parent.join("bin.new");
    let old = parent.join("bin.old");
    let io = |what: &str, e: std::io::Error| format!("{what}: {e}");
    let _ = std::fs::remove_dir_all(&new);
    let _ = std::fs::remove_dir_all(&old);
    std::fs::create_dir_all(&new).map_err(|e| io("bin.new", e))?;
    let mut listing = serde_json::Map::new();
    for f in &m.files {
        let dest = f.local_path(&new);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| io("bin.new", e))?;
        }
        std::fs::copy(f.local_path(staged), &dest).map_err(|e| io(&f.path, e))?;
        listing.insert(f.path.clone(), f.sha256.clone().into());
    }
    let listing = serde_json::to_string_pretty(&listing).unwrap_or_default() + "\n";
    std::fs::write(new.join("manifest.json"), listing).map_err(|e| io("manifest.json", e))?;
    if !verify_copy(&new, m) {
        return Err("the copied files do not match the update".into());
    }
    let mut last = String::new();
    for _ in 0..40 {
        match std::fs::rename(bin, &old) {
            Ok(()) => {
                return match std::fs::rename(&new, bin) {
                    Ok(()) => Ok(old),
                    Err(e) => {
                        let _ = std::fs::rename(&old, bin);
                        Err(io("installing the new bin", e))
                    }
                };
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!("bin is still in use: {last}"))
}

fn verify_copy(dir: &Path, m: &UpdateManifest) -> bool {
    m.files.iter().all(|f| file_matches(&f.local_path(dir), f))
}

/// Puts the old `bin` back after a failed update.
pub fn restore_bin(bin: &Path, old: &Path) -> Result<(), String> {
    let failed = bin.with_file_name("bin.failed");
    let _ = std::fs::remove_dir_all(&failed);
    for _ in 0..20 {
        if std::fs::rename(bin, &failed).is_ok() || !bin.exists() {
            return std::fs::rename(old, bin).map_err(|e| format!("restoring bin: {e}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("bin is in use; could not restore the previous version".into())
}

/// Copies the plugin in `bin` into every game where a different PlasmaTrack
/// plugin is installed. Games without the plugin are left alone (the player
/// may have removed it). Skipped while a game runs.
pub fn sync_plugins(bin: &Path, ini: &[(&str, String)]) -> Vec<setup::FolderResult> {
    let dll = bin.join(steam::PLUGIN_FILE_WINDOWS);
    if !dll.is_file() || games_running() {
        return Vec::new();
    }
    let Ok(want) = crate::manifest::sha256_file(&dll) else { return Vec::new() };
    let stale: Vec<steam::GameInstall> = steam::discover()
        .into_iter()
        .filter(|i| i.windows_build && i.plugin_path().is_file())
        .filter(|i| crate::manifest::sha256_file(&i.plugin_path()).ok().as_deref() != Some(want.as_str()))
        .collect();
    if stale.is_empty() {
        return Vec::new();
    }
    let results = setup::install_plugin(&stale, &dll, ini);
    for r in &results {
        match &r.error {
            None => log!("plugin updated in {}", r.plugins_dir.display()),
            Some(e) => log!("plugin not updated in {}: {e}", r.plugins_dir.display()),
        }
    }
    results
}

pub fn read_outcome(path: &Path) -> Option<Outcome> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn write_outcome(path: &Path, o: &Outcome) {
    let _ = std::fs::write(path, serde_json::to_string(o).unwrap_or_default());
}

/// Whether `version` failed to install recently (see [`FAILED_FILE`]).
pub fn failed_recently(data_dir: &Path, version: &str, now_ms: i64) -> Option<String> {
    let o = read_outcome(&data_dir.join(FAILED_FILE))?;
    (o.version == version && now_ms - o.at_ms < RETRY_FAILED.as_millis() as i64).then(|| o.error.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.0", "0.1.1"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("garbage", "0.1.0"));
    }

    #[test]
    fn cleans_installed_and_older_versions_only() {
        let dir = tempfile::tempdir().unwrap();
        for v in ["0.1.0", "0.2.0", "0.3.0", "notes"] {
            std::fs::create_dir_all(dir.path().join(v)).unwrap();
        }
        clean(dir.path(), "0.2.0");
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["0.3.0", "notes"]);
    }
}
