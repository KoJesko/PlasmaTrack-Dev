//! Command line shared by `plasmatrack-client` (console) and
//! `plasmatrack-clientw` (Windows, no console window; started at login by the
//! installer, logging to `client.log` in the data directory).
//!
//!   run              [--config PATH] [--data-dir PATH]
//!   init-config      [--config PATH]
//!   status           [--config PATH] [--data-dir PATH]
//!   find-games       [--json]
//!   write-ini        [--config PATH]
//!   install-plugin   --plugin PATH [--config PATH]   copy the plugin into every game found
//!   uninstall-plugin                                 remove it (and plasmatrack.ini) again
//!   set-local        [--config PATH]   reads {"server_url": .., "token": ..} (both optional)
//!                                      as JSON on stdin, so the token never shows up
//!                                      in a process list; creates the config if missing
//!   show-local       [--config PATH]   [local] as JSON, with the token only as set/unset

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pt_core::Config;

use crate::db::Db;
use crate::status::Status;
use crate::uploader::UploaderConfig;
use crate::{listener, log, manifest, paths, setup, status, steam, uploader};

/// Log file of the windowless client, in the data directory.
pub const LOG_FILE: &str = "client.log";
const LOG_ROTATE_BYTES: u64 = 8 << 20;

struct Args {
    command: String,
    config: PathBuf,
    data_dir: PathBuf,
    json: bool,
    plugin: Option<PathBuf>,
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
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = it.next().ok_or("--config needs a path")?.into(),
            "--data-dir" => args.data_dir = it.next().ok_or("--data-dir needs a path")?.into(),
            "--plugin" => args.plugin = Some(it.next().ok_or("--plugin needs a path")?.into()),
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

fn managed_ini(cfg: &Config) -> Vec<(&'static str, String)> {
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

async fn run(args: Args) -> Result<(), String> {
    let cfg = load_config(&args.config)?;
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

    let ip: IpAddr = cfg.local.listen_address.parse().map_err(|e| format!("listen_address: {e}"))?;
    let plugin_listener = tokio::net::TcpListener::bind(SocketAddr::new(ip, cfg.local.port))
        .await
        .map_err(|e| format!("cannot listen on {ip}:{} (port in use?): {e}", cfg.local.port))?;
    log!("waiting for the plugin on {ip}:{}", cfg.local.port);
    tokio::spawn(listener::serve(plugin_listener, db.clone(), status.clone()));

    let status_addr = SocketAddr::new(ip, cfg.local.status_port);
    {
        let (status, db) = (status.clone(), db.clone());
        tokio::spawn(async move {
            if let Err(e) = status::serve(status_addr, status, db).await {
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

    // Without a console (plasmatrack-clientw) there is no Ctrl+C; run until killed.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
    log!("shutting down");
    Ok(())
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
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => paths::SAMPLE_CONFIG.to_string(),
        Err(e) => return Err(format!("{}: {e}", args.config.display())),
    };
    let text = setup::set_local(&existing, input.server_url.as_deref(), input.token.as_deref())?;
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
        "run" => tokio::runtime::Runtime::new().map_err(|e| e.to_string()).and_then(|rt| rt.block_on(run(args))),
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
