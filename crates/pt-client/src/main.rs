//! `plasmatrack-client` command line.
//!
//!   plasmatrack-client run         [--config PATH] [--data-dir PATH]
//!   plasmatrack-client init-config [--config PATH]
//!   plasmatrack-client status      [--config PATH] [--data-dir PATH]
//!   plasmatrack-client find-games
//!   plasmatrack-client write-ini   [--config PATH]

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pt_client::db::Db;
use pt_client::status::Status;
use pt_client::uploader::UploaderConfig;
use pt_client::{listener, log, manifest, paths, status, steam, uploader};
use pt_core::Config;

struct Args {
    command: String,
    config: PathBuf,
    data_dir: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().unwrap_or_else(|| "run".into());
    let mut args = Args { command, config: paths::default_config_path(), data_dir: paths::default_data_dir() };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = it.next().ok_or("--config needs a path")?.into(),
            "--data-dir" => args.data_dir = it.next().ok_or("--data-dir needs a path")?.into(),
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
        client_version: pt_client::CLIENT_VERSION.into(),
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

    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    log!("shutting down");
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
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
            .map(|s| println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default()))
            .map_err(|e| e.to_string()),
        "find-games" => {
            for i in steam::discover() {
                println!("{:?}\t{}\tplugin installed: {}", i.game, i.plugins_dir.display(), i.plugin_path().is_file());
            }
            Ok(())
        }
        "write-ini" => load_config(&args.config).map(|cfg| write_inis(&cfg, &steam::discover())),
        other => Err(format!("unknown command {other:?}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
