# PlasmaTrack

Job tracker for Euro Truck Simulator 2 and American Truck Simulator with
server-side anti-cheat validation. ETS2 and ATS jobs are kept fully separate.

Status: foundation only. The design plan (`docs/PLAN.md`) is awaiting approval.

## Layout

| Path | What |
|------|------|
| `third_party/scssdk-1.14/` | SCS Telemetry SDK 1.14 headers, unmodified (MIT, see `PROVENANCE.md`) |
| `plugin/` | C++ telemetry plugin (Windows x64 DLL, also native Linux `.so`) — stage 1 |
| `crates/pt-core/` | Shared Rust core used by client **and** server: units, config, timeline, rules |
| `config/plasmatrack.sample.toml` | Every threshold, with units in the key names |
| `fixtures/` | Replay-harness traces and expected results |
| `scripts/` | Build / install / verification helpers |
| `docs/` | Plan and design notes |

## Build (Rust core)

Requires a stable Rust toolchain (https://rustup.rs).

    cargo test --workspace
