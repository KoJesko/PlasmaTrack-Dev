//! Command line shared by `plasmatrack-client` (console) and
//! `plasmatrack-clientw` (Windows, no console window; started at login by the
//! installer, logging to `client.log` in the data directory, with a tray icon).
//!
//!   run              [--config PATH] [--data-dir PATH]   a second `run` finds the first
//!                                      and (windowless) opens its status page
//!   stop             [--config PATH]   ask a running client to quit
//!   init-config      [--config PATH]
//!   status           [--config PATH] [--data-dir PATH]
//!   find-games       [--json]
//!   write-ini        [--config PATH]
//!   install-plugin   --plugin PATH [--config PATH]   copy the plugin into every game found
//!   uninstall-plugin                                 remove it (and plasmatrack.ini) again
//!   set-local        [--config PATH]   reads {"server_url": .., "token": ..} (both optional)
//!                                      as JSON on stdin, so the token never shows up
//!                                      in a process list; writes the config this build
//!                                      ships, keeping the existing [local] values
//!   show-local       [--config PATH]   [local] as JSON, with the token only as set/unset
//!   check-update     [--config PATH] [--data-dir PATH]   ask the server, download and
//!                                      verify a newer release without installing it
//!   apply-update     --bin PATH [--from VERSION] [--config PATH] [--data-dir PATH]
//!                                      run by the updater from the staged release
//!                                      (docs/UPDATES.md); installs it into PATH

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pt_core::Config;
use tokio::sync::Notify;

use crate::db::Db;
use crate::status::{SharedStatus, Status};
use crate::uploader::UploaderConfig;
use crate::live::{self, Alert, LiveJobs};
use crate::{gamelog, listener, log, manifest, paths, setup, status, steam, updater, uploader};

/// Log file of the windowless client, in the data directory.
pub const LOG_FILE: &str = "client.log";
const LOG_ROTATE_BYTES: u64 = 8 << 20;

struct Args {
    command: String,
    config: PathBuf,
    data_dir: PathBuf,
    json: bool,
    plugin: Option<PathBuf>,
    bin: Option<PathBuf>,
    from: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().unwrap_or_else(|| "run".into());
    let mut args = Args {
        command,
        config: paths::default_config_path(),
        data_dir: paths::default_data_dir(),
        json: false,
        plugin: None,
        bin: None,
        from: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = it.next().ok_or("--config needs a path")?.into(),
            "--data-dir" => args.data_dir = it.next().ok_or("--data-dir needs a path")?.into(),
            "--plugin" => args.plugin = Some(it.next().ok_or("--plugin needs a path")?.into()),
            "--bin" => args.bin = Some(it.next().ok_or("--bin needs a path")?.into()),
            "--from" => args.from = Some(it.next().ok_or("--from needs a version")?),
            "--json" => args.json = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

fn load_config(path: &PathBuf) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e} (run `plasmatrack-client init-config`)", path.display()))?;
    Config::from_toml_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn managed_ini(cfg: &Config) -> Vec<(&'static str, String)> {
    vec![
        ("port", cfg.local.port.to_string()),
        ("sample_rate_hz", cfg.plugin.sample_rate_hz.to_string()),
        ("replay_buffer_s", cfg.plugin.replay_buffer_wall_s.to_string()),
    ]
}

fn write_inis(cfg: &Config, installs: &[steam::GameInstall]) {
    for i in installs {
        if !i.plugin_path().is_file() {
            continue; // only where the plugin is installed
        }
        match steam::write_ini(&i.plugins_dir, &managed_ini(cfg)) {
            Ok(true) => log!("updated {}", i.plugins_dir.join(steam::INI_FILE).display()),
            Ok(false) => {}
            Err(e) => log!("cannot write ini in {}: {e}", i.plugins_dir.display()),
        }
    }
}

/// Links `run` with whoever started it (the tray): a shutdown request, and the
/// live state once the client is up.
#[derive(Clone, Default)]
pub struct Hooks {
    pub shutdown: Arc<Notify>,
    pub live: Arc<OnceLock<Live>>,
    /// Shows a desktop notification (title, text); the tray sets it.
    pub notify: Arc<OnceLock<NotifyFn>>,
    /// Keep this client up to date (the windowless client run from the
    /// installed `bin` folder; docs/UPDATES.md).
    pub auto_update: bool,
}

pub type NotifyFn = Box<dyn Fn(&str, &str) + Send + Sync>;

pub struct Live {
    pub status: SharedStatus,
    pub db: Arc<Mutex<Db>>,
    pub status_url: String,
}

enum RunError {
    /// Another client already listens; its status page.
    AlreadyRunning(String),
    Failed(String),
}

impl From<String> for RunError {
    fn from(e: String) -> Self {
        RunError::Failed(e)
    }
}

fn status_url(cfg: &Config) -> String {
    format!("http://{}:{}/", cfg.local.listen_address, cfg.local.status_port)
}

fn local_http() -> Option<reqwest::Client> {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().ok()
}

/// True when a PlasmaTrack client answers on the status page.
async fn client_answers(status_url: &str) -> bool {
    let Some(http) = local_http() else { return false };
    let Ok(res) = http.get(format!("{status_url}status.json")).send().await else { return false };
    if !res.status().is_success() {
        return false;
    }
    let body = res.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&body).is_ok_and(|v| v.get("client_version").is_some())
}

async fn run(args: Args, hooks: Hooks) -> Result<(), RunError> {
    let cfg = load_config(&args.config)?;
    let url = status_url(&cfg);
    let ip: IpAddr = cfg.local.listen_address.parse().map_err(|e| format!("listen_address: {e}"))?;
    let plugin_listener = match tokio::net::TcpListener::bind(SocketAddr::new(ip, cfg.local.port)).await {
        Ok(l) => l,
        Err(_) if client_answers(&url).await => return Err(RunError::AlreadyRunning(url)),
        Err(e) => return Err(format!("cannot listen on {ip}:{} (port in use?): {e}", cfg.local.port).into()),
    };
    std::fs::create_dir_all(&args.data_dir).map_err(|e| format!("{}: {e}", args.data_dir.display()))?;
    let db = Db::open(&args.data_dir.join(paths::DB_FILE)).map_err(|e| format!("database: {e}"))?;
    let db = Arc::new(Mutex::new(db));

    let installs = steam::discover();
    for i in &installs {
        log!("found {:?} plugins folder {} (plugin installed: {})", i.game, i.plugins_dir.display(), i.plugin_path().is_file());
    }
    write_inis(&cfg, &installs);
    let exe = std::env::current_exe().ok();
    let manifest = manifest::compute(exe.as_deref(), Some(&args.config), &installs);

    let status = Arc::new(Mutex::new(Status {
        client_version: crate::CLIENT_VERSION.into(),
        config_hash: cfg.integrity_hash(),
        manifest_files: manifest.len(),
        ..Default::default()
    }));
    let manifest = Arc::new(Mutex::new(manifest));

    // Provisional results and alerts (PLAN §6b), rebuilt from what is stored.
    let live_jobs = Arc::new(Mutex::new(LiveJobs::new(cfg.clone())));
    let (alert_tx, alert_rx) = tokio::sync::mpsc::unbounded_channel();
    let since = crate::now_ms() - (cfg.timeline.reopen_window_wall_h * 3_600_000.0) as i64;
    let feed = live::spawn_worker(live_jobs.clone(), db.clone(), since, alert_tx);
    tokio::spawn(show_alerts(alert_rx, cfg.clone(), hooks.notify.clone()));
    let libraries = steam::library_folders(&steam::steam_roots());
    tokio::spawn(gamelog::watch(db.clone(), Some(feed.clone()), libraries));

    log!("waiting for the plugin on {ip}:{}", cfg.local.port);
    tokio::spawn(listener::serve(plugin_listener, db.clone(), status.clone(), Some(feed)));

    let status_addr = SocketAddr::new(ip, cfg.local.status_port);
    {
        let (status, db, shutdown, jobs) = (status.clone(), db.clone(), hooks.shutdown.clone(), live_jobs.clone());
        tokio::spawn(async move {
            if let Err(e) = status::serve(status_addr, status, db, shutdown, Some(jobs)).await {
                log!("status page unavailable: {e}");
            }
        });
    }
    log!("status page: http://{status_addr}/");

    let ucfg = UploaderConfig {
        server_url: cfg.local.server_url.clone(),
        token: cfg.local.token.clone(),
        retry_interval: Duration::from_secs_f64(cfg.sync.retry_interval_wall_s),
        idle_poll: Duration::from_secs(2),
        max_frames: 5000,
        max_bytes: 4 << 20,
        truckersmp_module_names: cfg.multiplayer.truckersmp_module_names.clone(),
        config_hash: cfg.integrity_hash(),
        config_canonical_json: cfg.integrity_canonical_json(),
    };
    if !ucfg.is_configured() {
        log!("uploads paused until [local] server_url and token are set; everything is kept locally");
    }
    tokio::spawn(uploader::run(ucfg, db.clone(), status.clone(), manifest.clone()));
    if let (true, Some(bin)) = (hooks.auto_update, updater::installed_bin(&args.data_dir)) {
        if cfg.local.server_url.contains(".invalid") {
            log!("updates paused until [local] server_url is set");
        } else {
            let u = UpdateLoop { cfg: cfg.clone(), args: args.clone_paths(), bin, exe: exe.clone() };
            tokio::spawn(update_loop(u, status.clone(), manifest.clone(), hooks.shutdown.clone(), hooks.notify.clone()));
        }
    }
    let _ = hooks.live.set(Live { status, db, status_url: url });

    // Ctrl+C (console), POST /quit (`stop`, installer) or the tray's Quit.
    // Without a console, ctrl_c() may fail; then only the other two apply.
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = hooks.shutdown.notified() => {}
    }
    log!("shutting down");
    Ok(())
}

struct UpdateLoop {
    cfg: Config,
    args: Args,
    bin: PathBuf,
    exe: Option<PathBuf>,
}

impl Args {
    fn clone_paths(&self) -> Args {
        Args { command: String::new(), config: self.config.clone(), data_dir: self.data_dir.clone(), json: false, plugin: None, bin: None, from: None }
    }
}

const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const CHECK_AFTER_ERROR: Duration = Duration::from_secs(3600);
const UPDATE_TICK: Duration = Duration::from_secs(60);

fn http_for_updates() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent(format!("plasmatrack-client/{}", crate::CLIENT_VERSION))
        .build()
        .map_err(|e| e.to_string())
}

/// Starts a program of the client without a window, detached from this one.
fn launch(exe: &std::path::Path, args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let mut c = std::process::Command::new(exe);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    c.spawn().map(|_| ())
}

fn path_args(args: &Args) -> [&std::ffi::OsStr; 4] {
    ["--config".as_ref(), args.config.as_os_str(), "--data-dir".as_ref(), args.data_dir.as_os_str()]
}

/// Checks for updates, downloads them and, once no game runs, hands over to
/// `apply-update` and quits. Also keeps the games' plugin in step with `bin`.
async fn update_loop(u: UpdateLoop, status: SharedStatus, manifest: Arc<Mutex<Vec<pt_core::upload::ManifestEntry>>>, shutdown: Arc<Notify>, notify: Arc<OnceLock<NotifyFn>>) {
    let keys = pt_core::update::UPDATE_KEYS;
    let data_dir = u.args.data_dir.clone();
    let updates_dir = data_dir.join(updater::UPDATES_DIR);
    let Ok(http) = http_for_updates() else { return };
    let ini = managed_ini(&u.cfg);
    tokio::time::sleep(FIRST_CHECK).await;
    if let Some(o) = updater::read_outcome(&data_dir.join(updater::UPDATED_FILE)) {
        let _ = std::fs::remove_file(data_dir.join(updater::UPDATED_FILE));
        let text = format!("Updated from {} to {}.", o.from, o.version);
        log!("{text}");
        if let Some(show) = notify.get() {
            show("PlasmaTrack updated", &text);
        }
    }
    updater::clean(&updates_dir, crate::CLIENT_VERSION);
    let mut next_check = tokio::time::Instant::now();
    loop {
        let (bin, ini2) = (u.bin.clone(), ini.clone());
        let synced = tokio::task::spawn_blocking(move || updater::sync_plugins(&bin, &ini2)).await.unwrap_or_default();
        if synced.iter().any(|r| r.changed) {
            let installs = steam::discover();
            *manifest.lock().expect("manifest") = manifest::compute(u.exe.as_deref(), Some(&u.args.config), &installs);
        }
        let (dir, current) = (updates_dir.clone(), crate::CLIENT_VERSION);
        let ready = tokio::task::spawn_blocking(move || updater::staged(&dir, current, keys)).await.ok().flatten();
        if let Some((dir, m)) = ready {
            let failed = updater::failed_recently(&data_dir, &m.version, crate::now_ms());
            let connected = status.lock().map(|s| s.plugin_connections > 0).unwrap_or(true);
            let note = match &failed {
                Some(e) => format!("PlasmaTrack {} could not be installed ({e}); trying again later", m.version),
                None => format!("PlasmaTrack {} is ready and installs once ETS2 and ATS are closed", m.version),
            };
            if let Ok(mut s) = status.lock() {
                s.update = Some(note);
            }
            if failed.is_none() && !connected && !tokio::task::spawn_blocking(updater::games_running).await.unwrap_or(true) {
                let exe = dir.join("plasmatrack-clientw.exe");
                let mut a: Vec<&std::ffi::OsStr> = vec!["apply-update".as_ref(), "--bin".as_ref(), u.bin.as_os_str(), "--from".as_ref(), current.as_ref()];
                a.extend(path_args(&u.args));
                match launch(&exe, &a) {
                    Ok(()) => {
                        log!("installing PlasmaTrack {}; this client quits", m.version);
                        shutdown.notify_one();
                        return;
                    }
                    Err(e) => log!("cannot start {}: {e}", exe.display()),
                }
            }
        } else if tokio::time::Instant::now() >= next_check {
            next_check = tokio::time::Instant::now() + CHECK_EVERY;
            match updater::fetch_offer(&http, &u.cfg.local.server_url, crate::CLIENT_VERSION, keys).await {
                Ok(None) => {}
                Ok(Some(offer)) => {
                    log!("downloading PlasmaTrack {}", offer.manifest.version);
                    if let Ok(mut s) = status.lock() {
                        s.update = Some(format!("Downloading PlasmaTrack {}", offer.manifest.version));
                    }
                    if let Err(e) = updater::stage(&http, &u.cfg.local.server_url, &offer, &updates_dir).await {
                        log!("update download failed: {e}");
                        next_check = tokio::time::Instant::now() + CHECK_AFTER_ERROR;
                    }
                }
                Err(e) => {
                    log!("{e}");
                    next_check = tokio::time::Instant::now() + CHECK_AFTER_ERROR;
                }
            }
        }
        tokio::time::sleep(UPDATE_TICK).await;
    }
}

/// `apply-update`, run from the staged release (docs/UPDATES.md): waits for
/// the old client to quit, swaps `bin`, rebases the config, updates the
/// plugin and starts the new client. Anything that fails puts the old version
/// back and starts it again.
fn apply_update(args: &Args) -> Result<(), String> {
    let staged = std::env::current_exe().map_err(|e| e.to_string())?.parent().ok_or("no folder")?.to_path_buf();
    let bin = args.bin.clone().ok_or("apply-update needs --bin PATH")?;
    let from = args.from.clone().unwrap_or_else(|| "an older version".into());
    let m = updater::verify_staged(&staged, pt_core::update::UPDATE_KEYS)?;
    log!("apply-update: installing {} from {} into {}", m.version, staged.display(), bin.display());
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let url = load_config(&args.config).map(|c| status_url(&c)).unwrap_or_else(|_| "http://127.0.0.1:30456/".into());
    let start = |dir: &std::path::Path| {
        let mut a: Vec<&std::ffi::OsStr> = vec!["run".as_ref()];
        a.extend(path_args(args));
        launch(&dir.join("plasmatrack-clientw.exe"), &a)
    };
    let fail = |e: String| -> Result<(), String> {
        log!("apply-update: {e}");
        updater::write_outcome(&args.data_dir.join(updater::FAILED_FILE), &updater::Outcome {
            version: m.version.clone(), from: from.clone(), at_ms: crate::now_ms(), error: Some(e.clone()),
        });
        let _ = start(&bin);
        Err(e)
    };
    let gone = rt.block_on(async {
        for _ in 0..300 {
            if !client_answers(&url).await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    });
    if !gone {
        return fail("the running client did not quit".into());
    }
    if updater::games_running() {
        return fail("a game was started".into());
    }
    let old = match updater::swap_bin(&staged, &m, &bin) {
        Ok(old) => old,
        Err(e) => return fail(e),
    };
    // The config this release ships, with the player's [local] values.
    let old_config = std::fs::read_to_string(&args.config).ok();
    let rebased = setup::rebase_config(old_config.as_deref(), paths::SAMPLE_CONFIG).and_then(|text| {
        setup::write_atomically(&args.config, &text).map_err(|e| e.to_string())?;
        Config::from_toml_str(&text).map_err(|e| e.to_string())
    });
    let ini = rebased.as_ref().map(managed_ini).unwrap_or_default();
    updater::sync_plugins(&bin, &ini);
    let done = updater::Outcome { version: m.version.clone(), from: from.clone(), at_ms: crate::now_ms(), error: None };
    updater::write_outcome(&args.data_dir.join(updater::UPDATED_FILE), &done);
    let started = start(&bin).is_ok()
        && rt.block_on(async {
            for _ in 0..60 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if client_answers(&url).await {
                    return true;
                }
            }
            false
        });
    if started && rebased.is_ok() {
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_file(args.data_dir.join(updater::FAILED_FILE));
        log!("apply-update: PlasmaTrack {} is running", m.version);
        return Ok(());
    }
    // Back to the old version: its bin, its config, its plugin.
    let why = match rebased {
        Err(e) => format!("config: {e}"),
        Ok(_) => "the new client did not start".into(),
    };
    let _ = std::fs::remove_file(args.data_dir.join(updater::UPDATED_FILE));
    let _ = rt.block_on(async {
        let http = local_http()?;
        http.post(format!("{url}quit")).header(status::QUIT_HEADER.0, status::QUIT_HEADER.1).send().await.ok()
    });
    std::thread::sleep(Duration::from_secs(2));
    if let Err(e) = updater::restore_bin(&bin, &old) {
        log!("apply-update: {e}");
    }
    if let Some(text) = old_config {
        let _ = setup::write_atomically(&args.config, &text);
    }
    let old_ini = load_config(&args.config).map(|c| managed_ini(&c)).unwrap_or(ini);
    updater::sync_plugins(&bin, &old_ini);
    fail(why)
}

/// `check-update`: what the updater would download, verified, without
/// installing it. Prints the result as JSON.
fn check_update(args: &Args) -> Result<(), String> {
    let cfg = load_config(&args.config)?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let http = http_for_updates()?;
    let keys = pt_core::update::UPDATE_KEYS;
    let offer = rt.block_on(updater::fetch_offer(&http, &cfg.local.server_url, crate::CLIENT_VERSION, keys))?;
    let Some(offer) = offer else {
        print_json(&serde_json::json!({"current": crate::CLIENT_VERSION, "update": null}));
        return Ok(());
    };
    let dir = rt.block_on(updater::stage(&http, &cfg.local.server_url, &offer, &args.data_dir.join(updater::UPDATES_DIR)))?;
    let m = updater::verify_staged(&dir, keys)?;
    print_json(&serde_json::json!({"current": crate::CLIENT_VERSION, "update": m.version, "staged": dir, "files": m.files.len()}));
    Ok(())
}

/// Shows each alert after its random delay (PLAN decision 10), as a desktop
/// notification where there is a tray, and always in the log.
async fn show_alerts(mut rx: tokio::sync::mpsc::UnboundedReceiver<Alert>, cfg: Config, notify: Arc<OnceLock<NotifyFn>>) {
    while let Some(alert) = rx.recv().await {
        if !cfg.notifications.enabled {
            continue;
        }
        let delay = live::alert_delay(&cfg);
        let (cfg, notify) = (cfg.clone(), notify.clone());
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let (title, text) = alert.message(&cfg);
            log!("{title}: {text}");
            if let Some(show) = notify.get() {
                show(&title, &text);
            }
        });
    }
}

/// Tooltip and tray menu lines: game connection, then uploads.
pub fn status_lines(s: &Status, pending: Option<u64>) -> Vec<String> {
    let game = match s.last_game.as_deref() {
        Some("eut2") => "ETS2",
        Some("ats") => "ATS",
        _ => "the game",
    };
    let connection =
        if s.plugin_connections > 0 { format!("Connected to {game}") } else { "Waiting for ETS2 or ATS".to_string() };
    let failing = s.last_upload_error.is_some() && s.last_upload_ok_ms < s.last_upload_attempt_ms;
    let upload = match pending {
        _ if !s.upload_configured => Some("Uploads paused: no token".to_string()),
        _ if failing => Some("Upload failing, retrying".to_string()),
        Some(0) => Some("Everything uploaded".to_string()),
        Some(n) => Some(format!("{n} frames waiting to upload")),
        None => None,
    };
    std::iter::once(connection).chain(upload).collect()
}

/// `run` for plasmatrack-clientw on Windows: a tray icon while the client runs
/// on a worker thread; errors as a dialog, since there is no console.
#[cfg(windows)]
fn run_windowless(args: Args) -> Result<(), String> {
    use crate::tray;

    let cfg = match load_config(&args.config) {
        Ok(c) => c,
        Err(e) => {
            tray::error_box(&format!("PlasmaTrack could not start.\n\n{e}\n\nRun PlasmaTrack Setup to set it up."));
            return Err(e);
        }
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let url = status_url(&cfg);
    if rt.block_on(client_answers(&url)) {
        log!("already running; opening {url}");
        tray::open(&url);
        return Ok(());
    }

    let hooks = Hooks { auto_update: true, ..Hooks::default() };
    let (live, live2, shutdown) = (hooks.live.clone(), hooks.live.clone(), hooks.shutdown.clone());
    let data_dir = args.data_dir.clone();
    let cb = tray::Callbacks {
        title: format!("PlasmaTrack {}", crate::CLIENT_VERSION),
        lines: Box::new(move || match live.get() {
            Some(l) => {
                let status = l.status.lock().map(|s| s.clone()).unwrap_or_default();
                let pending = l.db.try_lock().ok().and_then(|db| db.stats().ok()).map(|s| s.frames_pending_upload);
                status_lines(&status, pending)
            }
            None => vec!["Starting…".to_string()],
        }),
        open_status: Box::new(move || {
            if let Some(l) = live2.get() {
                tray::open(&l.status_url);
            }
        }),
        open_data: Box::new(move || tray::open(&data_dir.to_string_lossy())),
        quit: Box::new(move || shutdown.notify_one()),
    };
    tray::run(cb, move |exit| {
        let notifier = exit.notifier();
        let _ = hooks.notify.set(Box::new(move |title: &str, text: &str| notifier.show(title, text)));
        std::thread::spawn(move || {
            match rt.block_on(run(args, hooks)) {
                Ok(()) => {}
                Err(RunError::AlreadyRunning(url)) => tray::open(&url),
                Err(RunError::Failed(e)) => {
                    log!("error: {e}");
                    tray::error_box(&format!("PlasmaTrack stopped.\n\n{e}"));
                }
            }
            exit.post();
        });
    })
}

#[cfg(not(windows))]
fn run_windowless(args: Args) -> Result<(), String> {
    run_console(args)
}

fn run_console(args: Args) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    match rt.block_on(run(args, Hooks::default())) {
        Ok(()) => Ok(()),
        Err(RunError::AlreadyRunning(url)) => {
            println!("PlasmaTrack is already running; status page: {url}");
            Ok(())
        }
        Err(RunError::Failed(e)) => Err(e),
    }
}

/// Asks a running client to quit and waits until it has.
fn stop(args: &Args) -> Result<(), String> {
    let cfg = load_config(&args.config)?;
    let url = status_url(&cfg);
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let http = local_http().ok_or("http client")?;
        let sent = http.post(format!("{url}quit")).header(status::QUIT_HEADER.0, status::QUIT_HEADER.1).send().await;
        match sent {
            Err(_) => {
                println!("not running");
                return Ok(());
            }
            Ok(r) if !r.status().is_success() => return Err(format!("quit refused: {}", r.status())),
            Ok(_) => {}
        }
        for _ in 0..50 {
            if !client_answers(&url).await {
                println!("stopped");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("the client did not stop within 5 s".to_string())
    })
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Fails when any folder failed, after printing every result.
fn report(results: Vec<setup::FolderResult>) -> Result<(), String> {
    let failed = results.iter().filter(|r| r.error.is_some()).count();
    print_json(&results);
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} game folder(s) failed")),
    }
}

#[derive(serde::Deserialize)]
struct LocalInput {
    server_url: Option<String>,
    token: Option<String>,
}

fn set_local(args: &Args) -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).map_err(|e| format!("stdin: {e}"))?;
    let input: LocalInput = serde_json::from_str(&input).map_err(|e| format!("stdin JSON: {e}"))?;
    let existing = match std::fs::read_to_string(&args.config) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", args.config.display())),
    };
    // Always the config this build ships, with the player's [local] kept.
    let rebased = setup::rebase_config(existing.as_deref(), paths::SAMPLE_CONFIG)?;
    let text = setup::set_local(&rebased, input.server_url.as_deref(), input.token.as_deref())?;
    setup::write_atomically(&args.config, &text).map_err(|e| format!("{}: {e}", args.config.display()))?;
    println!("wrote {}", args.config.display());
    Ok(())
}

fn show_local(args: &Args) -> Result<(), String> {
    let exists = args.config.is_file();
    let cfg = if exists { load_config(&args.config)? } else { Config::from_toml_str(paths::SAMPLE_CONFIG).map_err(|e| e.to_string())? };
    print_json(&serde_json::json!({
        "config_path": args.config,
        "config_exists": exists,
        "server_url": cfg.local.server_url,
        "token_set": !cfg.local.token.trim().is_empty(),
        "status_url": format!("http://{}:{}/", cfg.local.listen_address, cfg.local.status_port),
    }));
    Ok(())
}

fn install_plugin(args: &Args) -> Result<(), String> {
    let plugin = args.plugin.as_ref().ok_or("install-plugin needs --plugin PATH")?;
    if !plugin.is_file() {
        return Err(format!("{} not found", plugin.display()));
    }
    let cfg = load_config(&args.config)?;
    let installs = steam::discover();
    if installs.is_empty() {
        return Err("no ETS2 or ATS install found in any Steam library".into());
    }
    report(setup::install_plugin(&installs, plugin, &managed_ini(&cfg)))
}

/// Sends `log!` lines to `client.log` in the data directory, keeping one older
/// file once it grows past a few MiB.
fn log_to_file(data_dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(LOG_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(&path, data_dir.join(format!("{LOG_FILE}.old")));
    }
    crate::set_log_file(std::fs::OpenOptions::new().create(true).append(true).open(path)?);
    Ok(())
}

/// Entry point of both binaries. `windowless`: log to the data directory's
/// `client.log`, since there is no console to write to.
pub fn main(windowless: bool) -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if windowless {
        if let Err(e) = log_to_file(&args.data_dir) {
            eprintln!("cannot open {}: {e}", args.data_dir.join(LOG_FILE).display());
        }
    }
    let result = match args.command.as_str() {
        "run" if windowless => run_windowless(args),
        "run" => run_console(args),
        "stop" => stop(&args),
        "init-config" => {
            if args.config.exists() {
                Err(format!("{} already exists", args.config.display()))
            } else {
                std::fs::create_dir_all(args.config.parent().unwrap_or(&PathBuf::from(".")))
                    .and_then(|_| std::fs::write(&args.config, paths::SAMPLE_CONFIG))
                    .map(|_| println!("wrote {}", args.config.display()))
                    .map_err(|e| e.to_string())
            }
        }
        "status" => Db::open(&args.data_dir.join(paths::DB_FILE))
            .and_then(|db| db.stats())
            .map(|s| print_json(&s))
            .map_err(|e| e.to_string()),
        "find-games" => {
            let installs = steam::discover();
            if args.json {
                let rows: Vec<_> = installs
                    .iter()
                    .map(|i| {
                        serde_json::json!({
                            "game": i.game,
                            "plugins_dir": i.plugins_dir,
                            "windows_build": i.windows_build,
                            "plugin_installed": i.plugin_path().is_file(),
                        })
                    })
                    .collect();
                print_json(&rows);
            } else {
                for i in installs {
                    println!("{:?}\t{}\tplugin installed: {}", i.game, i.plugins_dir.display(), i.plugin_path().is_file());
                }
            }
            Ok(())
        }
        "write-ini" => load_config(&args.config).map(|cfg| write_inis(&cfg, &steam::discover())),
        "install-plugin" => install_plugin(&args),
        "uninstall-plugin" => report(setup::uninstall_plugin(&steam::discover())),
        "set-local" => set_local(&args),
        "show-local" => show_local(&args),
        "check-update" => check_update(&args),
        "apply-update" => apply_update(&args),
        other => Err(format!("unknown command {other:?}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if windowless {
                log!("error: {e}");
            }
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_describe_game_and_uploads() {
        let mut s = Status::default();
        assert_eq!(status_lines(&s, Some(3)), ["Waiting for ETS2 or ATS", "Uploads paused: no token"]);
        s.upload_configured = true;
        s.plugin_connections = 1;
        s.last_game = Some("eut2".into());
        assert_eq!(status_lines(&s, Some(0)), ["Connected to ETS2", "Everything uploaded"]);
        assert_eq!(status_lines(&s, Some(12)), ["Connected to ETS2", "12 frames waiting to upload"]);
        assert_eq!(status_lines(&s, None), ["Connected to ETS2"]);
        s.last_upload_attempt_ms = Some(20);
        s.last_upload_ok_ms = Some(10);
        s.last_upload_error = Some("503".into());
        assert_eq!(status_lines(&s, Some(12))[1], "Upload failing, retrying");
        s.last_upload_ok_ms = Some(20);
        assert_eq!(status_lines(&s, Some(0))[1], "Everything uploaded");
    }
}
