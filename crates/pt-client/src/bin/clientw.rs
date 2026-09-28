//! `plasmatrack-clientw`: the same commands as `plasmatrack-client`, without a
//! console window on Windows. The installer starts `plasmatrack-clientw run` at
//! login; log lines go to `client.log` in the data directory.

#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    pt_client::cli::main(true)
}
