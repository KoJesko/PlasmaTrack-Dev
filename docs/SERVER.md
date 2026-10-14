# The PlasmaTrack server

**Audience:** whoever deploys or operates the server. The design is in PLAN §7a; the API the
bot uses is in API_CONVENTIONS.md and its companions.

The server is a Cloudflare Worker (Workers Paid). Its logic is the `pt-server` crate, which is
tested natively (`cargo test -p pt-server`). `pt-worker` binds it to Cloudflare.

## Two deployments

| | Production | Test |
|---|---|---|
| Worker | `plasmatrack-server` | `plasmatrack-server-test` |
| URL | `https://plasmatrack-server.<account>.workers.dev` | `https://plasmatrack-server-test.<account>.workers.dev` |
| D1 | `plasmatrack` | `plasmatrack-test` |
| R2 | `plasmatrack-raw` | `plasmatrack-raw-test` |
| Queues | `plasmatrack-webhooks`, `plasmatrack-ops` | the same names with `-test` |
| Deploy | `npx wrangler deploy` | `npx wrangler deploy --env test` |

They share code and nothing else: data, tokens, webhook secrets and admins are all separate.
The test instance is seeded with the recorded captures (`scripts/seed-server.mjs`) and carries a
fake player, "Test Driver".

## Tools

- Rust with the `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`.
- `worker-build` 0.8.7 (Apache-2.0/MIT): `cargo install worker-build@0.8.7 --locked`. On first
  use it downloads `wasm-opt` (Binaryen, Apache-2.0) and `esbuild` (MIT).
- Node 22 or newer, then in `crates/pt-worker`: `npm ci`. That installs the pinned Wrangler
  (MIT/Apache-2.0).
- When running from VS Code, unset `ELECTRON_RUN_AS_NODE` first.

## Secrets

Each deployment has six secrets:

| Secret | What |
|---|---|
| `READ_TOKEN` | the bot's read token (API_CONVENTIONS §2) |
| `BOT_TOKEN` | the bot's bot token |
| `WEBHOOK_SECRETS` | the HMAC secret webhooks are signed with; `new,old` during rotation (WEBHOOKS §2) |
| `WEBHOOK_URL` | the bot's `/webhooks/plasmatrack` |
| `DOWNLOAD_SECRET` | signs trace download links |
| `BOOTSTRAP_ADMINS` | Discord IDs that are admins from their first call |

`node scripts/make-secrets.mjs --env production|test --webhook-url URL --admins ID[,ID…]` makes
the random ones and stores all six in Cloudflare. A copy stays in
`crates/pt-worker/.secrets.<env>.json`, which is gitignored.
- **The bot needs** three values from it: `BOT_TOKEN`, `READ_TOKEN` and `WEBHOOK_SECRETS`. Set
  them in the bot's own Worker with `wrangler secret put`.
- **Never** paste the file anywhere else or commit it.
- **Changing only the URL or the admins:** add `--keep` so the tokens stay the same.

## First deployment (done 2026-10-14)

From `crates/pt-worker`:

1. Log in: `npx wrangler login`.
2. Create the resources:
   - D1: `wrangler d1 create plasmatrack` and `plasmatrack-test`. Copy each `database_id` into
     `wrangler.toml`.
   - R2: `wrangler r2 bucket create plasmatrack-raw` and `plasmatrack-raw-test`.
   - Queues: `wrangler queues create` for the four queue names in the table above.
3. Create the tables: `wrangler d1 migrations apply plasmatrack --remote`, then the same for
   `plasmatrack-test` with `--env test`.
4. Deploy both Workers: `wrangler deploy`, then `wrangler deploy --env test`.
5. Set the secrets with `make-secrets.mjs` for both environments.
6. Publish the config and the release so jobs pass rule 0 (PLAN §6a), for both environments:
   `node scripts/publish-release.mjs --env production --url … --actor <admin Discord ID>`.

## Releasing a new client or plugin

1. Build the release: `cargo build --release -p pt-client` and the plugin, then
   `npm run dist` in `installer/`. That writes `installer/payload/manifest.json`.
2. Publish its hashes with `publish-release.mjs` to both servers.
   - **Without this step, every job from the new build is discarded.**
   - A config change publishes a new config version the same way.
3. To retire the old build, a moderator schedules a revocation through the bot (ADMIN_API §11).
   It takes effect 21 days later.

## Changing the server

1. `cargo test --workspace` must pass.
2. Deploy test first: `npx wrangler deploy --env test`. Try it through the bot on the test
   Discord server.
3. Then deploy production: `npx wrangler deploy`.
4. **A new migration** (`crates/pt-server/migrations/000N_*.sql`) is applied with
   `wrangler d1 migrations apply … --remote` *before* deploying the code that needs it.

## Running it locally

From `crates/pt-worker`:

1. Put local values in `.dev.vars` (gitignored), one `NAME=value` per line for the six secrets.
2. `npx wrangler d1 migrations apply plasmatrack --local`
3. `npx wrangler dev --local`
4. Seed it: `node scripts/seed-server.mjs --url http://127.0.0.1:8787 --bot-token … --actor … --publish <captures>`

**Stopping it:** stopping the terminal command does not always stop Wrangler's `workerd`
processes, and two local instances share one queue. Check for leftover `workerd.exe` processes.

## Watching it

- Live logs: `npx wrangler tail [--env test]`.
- Data: `npx wrangler d1 execute plasmatrack --remote --command "SELECT …"`.
- Undelivered webhooks: `SELECT type, attempts, last_error FROM webhook_outbox WHERE delivered_at IS NULL`.
