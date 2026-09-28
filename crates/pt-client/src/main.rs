//! `plasmatrack-client` command line; see [`pt_client::cli`].

fn main() -> std::process::ExitCode {
    pt_client::cli::main(false)
}
