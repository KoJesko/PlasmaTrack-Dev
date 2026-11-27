//! The Linux package's install.sh / uninstall.sh (packaging/linux) with the
//! real client binary, against a fake home folder and Steam libraries.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn write(path: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Ports nothing listens on, so the scripts' `stop` cannot reach a client that
/// is really running on this PC.
fn free_ports() -> (u16, u16) {
    let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    (a.local_addr().unwrap().port(), b.local_addr().unwrap().port())
}

struct Fake {
    _dir: tempfile::TempDir,
    home: PathBuf,
    package: PathBuf,
    games: Vec<PathBuf>,
}

impl Fake {
    fn new() -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (home, package) = (root.join("home"), root.join("package"));
        let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux");
        std::fs::create_dir_all(&package).unwrap();
        for f in ["install.sh", "uninstall.sh", "plasmatrack-client.service"] {
            std::fs::copy(sources.join(f), package.join(f)).unwrap();
        }
        std::fs::copy(env!("CARGO_BIN_EXE_plasmatrack-client"), package.join("plasmatrack-client")).unwrap();
        write(&package.join("plasmatrack.dll"), "dll", 0o644);
        // systemctl: records what it was asked.
        write(&root.join("stubs/systemctl"), "#!/bin/sh\necho \"$@\" >> \"$HOME/systemctl.log\"\n", 0o755);

        // ETS2 in the default library, ATS in a second one (libraryfolders.vdf).
        let steam = home.join(".local/share/Steam");
        let other = root.join("games");
        let games = vec![
            steam.join("steamapps/common/Euro Truck Simulator 2/bin/win_x64"),
            other.join("steamapps/common/American Truck Simulator/bin/win_x64"),
        ];
        games.iter().for_each(|g| std::fs::create_dir_all(g).unwrap());
        write(&steam.join("steamapps/libraryfolders.vdf"), &format!("\"path\"\t\t\"{}\"\n", other.display()), 0o644);

        let (port, status_port) = free_ports();
        let config = include_str!("../../../config/plasmatrack.sample.toml")
            .replace("port = 30455", &format!("port = {port}"))
            .replace("status_port = 30456", &format!("status_port = {status_port}"));
        write(&home.join(".config/plasmatrack/plasmatrack.toml"), &config, 0o644);
        Fake { _dir: dir, home, package, games }
    }

    fn run(&self, script: &Path, args: &[&str], token: &str) -> Output {
        let stubs = self.package.parent().unwrap().join("stubs");
        let out = Command::new("bash")
            .arg(script)
            .args(args)
            .env("HOME", &self.home)
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env("PLASMATRACK_TOKEN", token)
            .env("PATH", format!("{}:{}", stubs.display(), std::env::var("PATH").unwrap()))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{script:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }
}

#[test]
fn installs_upgrades_and_uninstalls() {
    let f = Fake::new();
    let install = f.package.join("install.sh");
    f.run(&install, &["--server-url", "https://pt.example/"], "tok-\"quoted\\123");

    let bin = f.home.join(".local/share/plasmatrack/bin");
    let config = f.home.join(".config/plasmatrack/plasmatrack.toml");
    let unit = f.home.join(".config/systemd/user/plasmatrack-client.service");
    assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
    let cfg = pt_core::Config::from_toml_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!((cfg.local.server_url.as_str(), cfg.local.token.as_str()), ("https://pt.example", "tok-\"quoted\\123"));
    assert_eq!(
        std::fs::read(bin.join("plasmatrack-client")).unwrap(),
        std::fs::read(f.package.join("plasmatrack-client")).unwrap(),
        "the published bytes, so rule 0 matches"
    );
    for g in &f.games {
        assert_eq!(std::fs::read_to_string(g.join("plugins/plasmatrack.dll")).unwrap(), "dll");
        assert!(g.join("plugins/plasmatrack.ini").is_file());
    }
    let service = std::fs::read_to_string(&unit).unwrap();
    assert!(service.contains(&format!("ExecStart=\"{}\" run", bin.join("plasmatrack-client").display())), "{service}");
    let log = std::fs::read_to_string(f.home.join("systemctl.log")).unwrap();
    assert!(log.contains("--user enable plasmatrack-client.service") && log.contains("--user restart"), "{log}");

    // Upgrading without a token or URL keeps both.
    f.run(&install, &[], "");
    let again = pt_core::Config::from_toml_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!((again.local.server_url, again.local.token), (cfg.local.server_url, cfg.local.token));

    // An older config that no longer loads does not block an upgrade.
    let old = std::fs::read_to_string(&config).unwrap().replace("config_version = 1", "");
    std::fs::write(&config, old).unwrap();
    f.run(&install, &[], "");
    let kept = pt_core::Config::from_toml_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(kept.local.token, "tok-\"quoted\\123");

    // The installed copy of uninstall.sh removes itself with the program.
    f.run(&bin.join("uninstall.sh"), &[], "");
    assert!(!bin.exists() && !unit.exists() && config.exists());
    for g in &f.games {
        assert!(!g.join("plugins/plasmatrack.dll").exists() && !g.join("plugins/plasmatrack.ini").exists());
    }
    // From the package, with nothing installed: still clean.
    f.run(&f.package.join("uninstall.sh"), &["--delete-data"], "");
    assert!(!config.parent().unwrap().exists() && !bin.parent().unwrap().exists());
}
