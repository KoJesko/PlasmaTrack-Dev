//! `plasmatrack-uninstall` (published as `PlasmaTrack-Uninstall.exe`): removes
//! PlasmaTrack from this PC; see [`pt_client::uninstall`]. Anyone may download
//! it, and Windows starts it from "Installed apps".

#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    pt_client::uninstall::gui_main()
}
