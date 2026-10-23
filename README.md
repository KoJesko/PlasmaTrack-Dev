# PlasmaTrack

Job tracker for Euro Truck Simulator 2 and American Truck Simulator. A game plugin
records telemetry, a background client uploads it, and the server rebuilds each job
and scores it, with anti-cheat checks done server-side. ETS2 and ATS jobs are kept
fully separate.

Status: beta (client 0.2.5).

## How it fits together

1. The plugin (a DLL loaded by the game) streams telemetry to the client over TCP on loopback.
2. The client keeps a local SQLite log and uploads raw chunks to the server.
3. The server, on Cloudflare Workers, replays each player's chunks through the shared
   timeline and rules, stores the scored jobs, and serves them to the Discord bot through
   the public and admin APIs.

The client and the server use the same Rust core (`pt-core`), so a job is scored the
same way wherever it is replayed.

## Layout

| Path | What |
|------|------|
| `plugin/` | C++ telemetry plugin (Windows x64 DLL, also a native Linux `.so`) |
| `crates/pt-core/` | Shared core: units, config, timeline reconstruction, scoring rules |
| `crates/pt-client/` | Background client: receives telemetry, keeps the local log, uploads, self-updates |
| `crates/pt-server/` | Server logic: ingest, scoring, public and admin APIs, webhooks (tested natively) |
| `crates/pt-worker/` | Cloudflare Workers binding for `pt-server` (D1, R2, Queues, Durable Object, Cron) |
| `crates/pt-replay/` | Replays captures and fixtures through the timeline and rules |
| `installer/` | Electron setup program that installs the plugin and client |
| `config/plasmatrack.sample.toml` | Every threshold, with units in the key names |
| `scripts/` | Build, release and server-seeding helpers |
| `third_party/` | SCS Telemetry SDK 1.14 and TruckersMP client SDK headers, unmodified, plus toolchain notices |
| `docs/` | Design plan, API references, build, install and server guides |

## Build

Rust workspace (needs a stable toolchain from https://rustup.rs and a C compiler for SQLite):

    cargo test --workspace

The plugin, the Windows toolchain and the installer have their own steps in
[`docs/BUILD.md`](docs/BUILD.md). Deploying the server is covered in
[`docs/SERVER.md`](docs/SERVER.md), and publishing a client release in
[`docs/UPDATES.md`](docs/UPDATES.md).

## Docs

- [`docs/INSTALL.md`](docs/INSTALL.md): installing the plugin and client
- [`docs/CLIENT.md`](docs/CLIENT.md): what the client does on a player's machine
- [`docs/PUBLIC_API.md`](docs/PUBLIC_API.md) and [`docs/ADMIN_API.md`](docs/ADMIN_API.md): the API the Discord bot uses
- [`docs/WEBHOOKS.md`](docs/WEBHOOKS.md): events the server sends to the bot
- [`docs/PLAN.md`](docs/PLAN.md): the original design plan

## License

PolyForm Noncommercial 1.0.0: reuse and modification allowed, no commercial use. See
`LICENSE.md` and `NOTICE`. Third-party components keep their own licenses (see
`THIRD_PARTY_NOTICES.md`).
