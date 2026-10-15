//! Removing PlasmaTrack from a PC, and its entry in Windows' "Installed apps".
//!
//! Three ways in, all running [`uninstall`]:
//! - `PlasmaTrack-Uninstall.exe` ([`gui_main`]), which anyone can download and
//!   which Windows starts from "Installed apps";
//! - `plasmatrack-client uninstall [--delete-data] [--json]`;
//! - PlasmaTrack Setup's Uninstall button, which runs that command.
//!
//! It works with nothing installed, and with only parts of an install left.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::{paths, setup, steam, updater};

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PlasmaTrack`.
pub const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\PlasmaTrack";
pub const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
pub const RUN_VALUE: &str = "PlasmaTrack";
/// The uninstaller's name in `bin`.
pub const UNINSTALLER_EXE: &str = "plasmatrack-uninstall.exe";
const CLIENT_PROCESSES: [&str; 2] = ["plasmatrack-clientw.exe", "plasmatrack-client.exe"];
/// Start menu shortcuts made by Setup.
const SHORTCUTS: [&str; 2] = ["PlasmaTrack.lnk", "Uninstall PlasmaTrack.lnk"];
/// What the updater leaves next to `bin` (updater.rs).
const LEFTOVERS: [&str; 5] = ["bin", "updates", "bin.new", "bin.old", "bin.failed"];

/// One thing done, as Setup shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub ok: bool,
    pub text: String,
    pub detail: Option<String>,
}

fn step(ok: bool, text: impl Into<String>, detail: Option<String>) -> Step {
    Step { ok, text: text.into(), detail }
}

/// Where the config and data are.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data_dir: PathBuf,
}

impl Default for Paths {
    fn default() -> Paths {
        Paths { config: paths::default_config_path(), data_dir: paths::default_data_dir() }
    }
}

/// Asks a running client to quit through its status page, like
/// `plasmatrack-client stop`, without an HTTP library.
fn ask_to_quit(config: &Path) {
    use std::io::Write;
    let cfg = std::fs::read_to_string(config).ok().and_then(|t| pt_core::Config::from_toml_str(&t).ok());
    let (host, port) = cfg.map_or(("127.0.0.1".to_string(), 30456), |c| (c.local.listen_address, c.local.status_port));
    let Ok(addr) = format!("{host}:{port}").parse::<std::net::SocketAddr>() else { return };
    if let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let (k, v) = crate::status::QUIT_HEADER;
        let _ = write!(s, "POST /quit HTTP/1.1\r\nHost: {host}\r\n{k}: {v}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = std::io::Read::read(&mut s, &mut [0u8; 256]);
    }
}

/// Stops every PlasmaTrack client: politely first, then by force.
fn stop_clients(config: &Path) -> Step {
    let running = || updater::processes(&CLIENT_PROCESSES).unwrap_or_default();
    if running().is_empty() {
        return step(true, "PlasmaTrack was not running", None);
    }
    ask_to_quit(config);
    for _ in 0..50 {
        if running().is_empty() {
            return step(true, "Stopped PlasmaTrack", None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for pid in running() {
        kill(pid);
    }
    std::thread::sleep(Duration::from_millis(500));
    match running().as_slice() {
        [] => step(true, "Stopped PlasmaTrack", None),
        left => step(false, "Could not stop PlasmaTrack", Some(format!("still running: process {left:?}"))),
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill(pid: u32) {
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
}

/// Removes a folder of ours; a folder that is not there counts as removed.
fn remove_dir(dir: &Path) -> Result<bool, String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// Only folders named like ours are deleted wholesale, whatever a path
/// argument says.
fn ours(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("plasmatrack"))
}

#[cfg(windows)]
fn start_menu() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Microsoft").join("Windows").join("Start Menu").join("Programs"))
}

#[cfg(windows)]
fn unregister_windows() -> Vec<Step> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let mut out = Vec::new();
    if let Ok(run) = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
        if run.delete_value(RUN_VALUE).is_ok() {
            out.push(step(true, "Removed PlasmaTrack from startup", None));
        }
    }
    let mut links = 0;
    for name in SHORTCUTS {
        if let Some(p) = start_menu().map(|d| d.join(name)) {
            links += std::fs::remove_file(p).is_ok() as usize;
        }
    }
    if links > 0 {
        out.push(step(true, "Removed PlasmaTrack from the Start menu", None));
    }
    if hkcu.delete_subkey_all(UNINSTALL_KEY).is_ok() {
        out.push(step(true, "Removed PlasmaTrack from Installed apps", None));
    }
    out
}

#[cfg(not(windows))]
fn unregister_windows() -> Vec<Step> {
    Vec::new()
}

/// Removes everything PlasmaTrack put on this PC. Local data, settings and the
/// token stay unless `delete_data`. Every step runs even when an earlier one
/// failed; the caller shows the list.
pub fn uninstall(p: &Paths, delete_data: bool) -> Vec<Step> {
    let mut steps = vec![stop_clients(&p.config)];
    steps.extend(unregister_windows());

    let folders = setup::uninstall_plugin(&steam::discover());
    for f in folders.iter().filter(|f| f.changed || f.error.is_some()) {
        let game = match f.game {
            pt_core::Game::Ats => "American Truck Simulator",
            pt_core::Game::Ets2 => "Euro Truck Simulator 2",
        };
        steps.push(match &f.error {
            None => step(true, format!("Removed the plugin from {game}"), Some(f.plugins_dir.display().to_string())),
            Some(e) => step(false, format!("Could not remove the plugin from {game}"), Some(e.clone())),
        });
    }

    steps.extend(remove_files(p, delete_data));
    steps
}

/// The file part of [`uninstall`]: program files, and local data when asked.
/// It touches nothing outside `p` (the tests run it).
pub fn remove_files(p: &Paths, delete_data: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut removed = false;
    for name in LEFTOVERS {
        match remove_dir(&p.data_dir.join(name)) {
            Ok(r) => removed |= r,
            Err(e) => steps.push(step(false, format!("Could not remove PlasmaTrack's {name} folder"), Some(e))),
        }
    }
    if removed {
        steps.push(step(true, "Removed the PlasmaTrack program files", Some(p.data_dir.join("bin").display().to_string())));
    }

    if delete_data {
        let config_dir = p.config.parent().map(Path::to_path_buf);
        for dir in [Some(p.data_dir.clone()), config_dir].into_iter().flatten() {
            let result = if ours(&dir) { remove_dir(&dir).map(|_| ()) } else { Err(format!("{} is not a PlasmaTrack folder", dir.display())) };
            if let Err(e) = result {
                steps.push(step(false, "Could not delete all local data", Some(e)));
            }
        }
        if ours(&p.data_dir) && !p.data_dir.exists() {
            steps.push(step(true, "Deleted local data, settings and the token", None));
        }
    } else if p.data_dir.exists() || p.config.exists() {
        steps.push(step(
            true,
            "Kept local data and settings",
            Some(format!("{} and {}", p.data_dir.display(), p.config.display())),
        ));
    }
    steps
}

/// Adds or refreshes PlasmaTrack in "Installed apps" (Setup, and every update):
/// the uninstaller in `bin`, the version, the size.
#[cfg(windows)]
pub fn register(bin: &Path, version: &str) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(UNINSTALL_KEY).map_err(|e| e.to_string())?;
    let uninstaller = bin.join(UNINSTALLER_EXE);
    let size_kb: u64 = std::fs::read_dir(bin).into_iter().flatten().flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>() / 1024;
    let s = |name: &str, v: &str| key.set_value(name, &v.to_string()).map_err(|e| e.to_string());
    let d = |name: &str, v: u32| key.set_value(name, &v).map_err(|e| e.to_string());
    s("DisplayName", "PlasmaTrack")?;
    s("DisplayVersion", version)?;
    s("Publisher", "PlasmaTrack")?;
    s("DisplayIcon", &bin.join("plasmatrack-clientw.exe").display().to_string())?;
    s("InstallLocation", &bin.display().to_string())?;
    s("UninstallString", &format!("\"{}\"", uninstaller.display()))?;
    s("QuietUninstallString", &format!("\"{}\" --quiet", uninstaller.display()))?;
    d("EstimatedSize", size_kb.min(u32::MAX as u64) as u32)?;
    d("NoModify", 1)?;
    d("NoRepair", 1)?;
    Ok(())
}

#[cfg(not(windows))]
pub fn register(_bin: &Path, _version: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
mod gui {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDRETRY, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK,
        MB_RETRYCANCEL, MB_YESNO,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn show(text: &str, flags: u32) -> i32 {
        let (text, title) = (wide(text), wide("Uninstall PlasmaTrack"));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags) }
    }

    pub fn yes(text: &str, default_no: bool) -> bool {
        show(text, MB_YESNO | MB_ICONQUESTION | if default_no { MB_DEFBUTTON2 } else { 0 }) == IDYES
    }

    pub fn retry(text: &str) -> bool {
        show(text, MB_RETRYCANCEL | MB_ICONWARNING) == IDRETRY
    }

    pub fn info(text: &str, failed: bool) {
        show(text, MB_OK | if failed { MB_ICONERROR } else { MB_ICONINFORMATION });
    }
}

/// When this program runs from the `bin` folder it is about to delete, it
/// continues from a copy in the temp folder instead.
#[cfg(windows)]
fn relocate(data_dir: &Path) -> Option<std::process::ExitCode> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().ok()?;
    let inside = exe.parent().zip(data_dir.join("bin").canonicalize().ok()).is_some_and(|(p, bin)| p.canonicalize().is_ok_and(|p| p == bin));
    if !inside {
        return None;
    }
    let copy = std::env::temp_dir().join(format!("PlasmaTrack-Uninstall-{}.exe", std::process::id()));
    std::fs::copy(&exe, &copy).ok()?;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let args: Vec<String> = std::env::args().skip(1).chain(std::iter::once("--relocated".to_string())).collect();
    std::process::Command::new(&copy).args(args).creation_flags(DETACHED_PROCESS).spawn().ok()?;
    Some(std::process::ExitCode::SUCCESS)
}

/// `PlasmaTrack-Uninstall.exe [--quiet] [--delete-data]`: asks, removes, and
/// says what it did. `--quiet` asks nothing and keeps local data unless
/// `--delete-data` is given.
#[cfg(windows)]
pub fn gui_main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x == a);
    let quiet = has("--quiet");
    let p = Paths::default();
    if !has("--relocated") {
        if let Some(code) = relocate(&p.data_dir) {
            return code;
        }
    }
    if !quiet
        && !gui::yes(
            "Remove PlasmaTrack from this PC?\n\nThis stops PlasmaTrack, removes its plugin from Euro Truck Simulator 2 and \
             American Truck Simulator, and removes it from startup and the Start menu.",
            false,
        )
    {
        return ExitCode::SUCCESS;
    }
    while updater::games_running() {
        if quiet || !gui::retry("Close Euro Truck Simulator 2 and American Truck Simulator first, then choose Retry.") {
            return ExitCode::FAILURE;
        }
    }
    let delete_data = has("--delete-data")
        || (!quiet
            && gui::yes(
                "Also delete PlasmaTrack's local data and settings, including your token?\n\nJobs already uploaded stay on the \
                 server. Choose No to keep them for a later reinstall.",
                true,
            ));
    let steps = uninstall(&p, delete_data);
    let failed: Vec<&Step> = steps.iter().filter(|s| !s.ok).collect();
    if !quiet {
        let text = if failed.is_empty() {
            "PlasmaTrack was removed from this PC.".to_string()
        } else {
            let lines: Vec<String> =
                failed.iter().map(|s| format!("• {}{}", s.text, s.detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())).collect();
            format!("PlasmaTrack was removed, except:\n\n{}\n\nClose the games and run this again.", lines.join("\n"))
        };
        gui::info(&text, !failed.is_empty());
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(not(windows))]
pub fn gui_main() -> std::process::ExitCode {
    let steps = uninstall(&Paths::default(), std::env::args().any(|a| a == "--delete-data"));
    println!("{}", serde_json::to_string_pretty(&steps).unwrap_or_default());
    if steps.iter().all(|s| s.ok) {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_program_files_and_keeps_or_deletes_data() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("local").join("PlasmaTrack");
        let config = root.path().join("roaming").join("PlasmaTrack").join("plasmatrack.toml");
        for d in ["bin/licenses", "updates/0.3.0", "bin.old"] {
            std::fs::create_dir_all(data.join(d)).unwrap();
        }
        std::fs::write(data.join("bin").join("plasmatrack-clientw.exe"), "x").unwrap();
        std::fs::write(data.join("client.sqlite"), "db").unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "token = 'x'").unwrap();
        let p = Paths { config: config.clone(), data_dir: data.clone() };

        let steps = remove_files(&p, false);
        for d in ["bin", "updates", "bin.old"] {
            assert!(!data.join(d).exists(), "{d}");
        }
        assert!(data.join("client.sqlite").exists() && config.exists(), "data kept");
        assert!(steps.iter().any(|s| s.text == "Kept local data and settings"));

        // Again, deleting data; then once more with nothing left: still fine.
        remove_files(&p, true);
        assert!(!data.exists() && !config.parent().unwrap().exists());
        let steps = remove_files(&p, true);
        assert!(steps.iter().all(|s| s.ok), "{steps:?}");
    }

    #[test]
    fn deletes_only_folders_named_plasmatrack() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Documents");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("precious.txt"), "keep").unwrap();
        let p = Paths { config: root.path().join("cfg.toml"), data_dir: data.clone() };
        let steps = remove_files(&p, true);
        assert!(data.join("precious.txt").exists());
        assert!(steps.iter().any(|s| !s.ok && s.detail.as_deref().is_some_and(|d| d.contains("not a PlasmaTrack folder"))));
    }
}
