# Client updates

PlasmaTrack keeps itself up to date. Players install once with PlasmaTrack Setup; after that,
every new release installs itself the next time ETS2 and ATS are both closed.

## What the player sees

- Nothing while they play. A game that is running is never interrupted, and its plugin is never
  replaced under it.
- After the games close: the tray icon disappears for a few seconds and comes back, followed by
  a "PlasmaTrack updated" notification ("Updated from 0.2.0 to 0.2.1.").
- The status page shows a line under `update` while a release is downloading or waiting for the
  games to close, and after a failed attempt.
- Jobs recorded before the update upload as usual afterwards. Both releases are published, so
  rule 0 accepts them.

## How it works

1. **Check.** The windowless client checks one minute after it starts, then every 6 hours (every
   hour after an error), with `GET {server_url}/api/v1/client/update?platform=windows&version=0.2.0`.
   It sends no token, so a player without one still gets updates.
2. **Verify.** The answer carries an update manifest and its Ed25519 signature:
   - The manifest lists every file of the `bin` folder with its SHA-256, size and compressed size.
   - The client installs only a manifest signed by a release key compiled into it
     (`pt_core::update::UPDATE_KEYS`).
   - The manifest must be for its platform and must be newer than the running version.
   - The server checks the signature as well, when the release is published, but it cannot sign.
     A compromised server or bot token cannot ship code to players.
3. **Download.** The files come gzip-compressed from `/dl/releases/windows/<version>/<path>.gz`,
   about 8.5 MB in all. Each one is checked against the manifest's size and SHA-256.
   - They are kept in `%LOCALAPPDATA%\PlasmaTrack\updates\<version>\`.
   - An interrupted download resumes: files already there and correct are kept.
4. **Install,** once no game runs and the plugin is not connected. The client starts
   `plasmatrack-clientw apply-update` from the downloaded folder and quits. That new program then:
   - waits for the old client to exit;
   - checks every file again;
   - swaps the folders `bin` → `bin.old` and the new files → `bin`;
   - rebuilds the config from the one it ships, keeping the player's `[local]` (server, token,
     ports), exactly as Setup does;
   - copies the new plugin into each game that has the plugin;
   - starts the new client, and waits for its status page.
5. **Rollback.** If the new client does not come up within 30 seconds, the old `bin`, the old
   config and the old plugin are put back, and the old client is started.
   - The failure is written to `update-failed.json` and shown on the status page.
   - That version is tried again after 6 hours.

**Only the installed client updates itself:** the windowless `plasmatrack-clientw run` started
from `%LOCALAPPDATA%\PlasmaTrack\bin` (the installer's location). A client run from a build
folder, or the console client, never does. `plasmatrack-client check-update` downloads and
verifies what the server offers without installing it.

**The plugin.** The client also copies `bin\plasmatrack.dll` into any game whose PlasmaTrack
plugin differs from it, whenever no game runs. That finishes an update whose plugin copy was
blocked by a game starting at the wrong moment. Games without the plugin are left alone.

**0.1.x has no updater.** Players on 0.1.0 or 0.1.1 run the 0.2.0 setup once, by hand.

## The release key

- **Private key:** `.release-signing-key.pem` in the repository root (gitignored), made by
  `node scripts/make-release-key.mjs`.
- **Public key:** `7dd1f8d9…e256e073` in `crates/pt-core/src/update.rs`.

**Keep an offline copy of the private key**, for example on a USB stick.
- Without it, no update can be signed for the clients already installed. Every player would
  then have to run a new setup by hand.
- If it leaks, anyone who also controls the server could ship code to players.

To replace the key:
1. Add the new public key to `UPDATE_KEYS`.
2. Ship a release signed with the old key.
3. After that, sign with the new key, and drop the old key in a later release.

## Publishing a release

1. Bump the version in `crates/pt-client/Cargo.toml` and `installer/package.json`.
2. Build:
   - `cargo build --release -p pt-client`;
   - the plugin, if it changed;
   - then `npm run dist` in `installer/` (with `ELECTRON_RUN_AS_NODE` unset).
3. Publish to test, with the upload:

   ```sh
   node scripts/publish-release.mjs --env test --url https://plasmatrack-server-test.jansherremway.workers.dev \
     --actor <admin Discord ID> --upload --notes "What changed, for players"
   ```

   - **`--upload`** gzips the payload files and puts them, and the setup program, in the R2
     bucket `plasmatrack-releases`. Both servers share that bucket, so it is needed once.
   - **The signed manifest** and the `.gz` files are kept in `installer/dist/update-<version>/`,
     so both servers get the same bytes.
   - **The release** is published with its file hashes (rule 0), the update, the setup program
     and the notes. It is offered at once, unless `--no-offer` is given.
4. Check it on test, then run the same command with `--env production` and the production URL,
   without `--upload`.

**Stopping a bad release:** `POST /api/v1/admin/releases/{release_id}/offer` with
`{"offered": false, "reason": …}` (ADMIN_API §11). Clients then get the newest release still
offered.
- A scheduled revocation of a release also stops its offer at once.
- Clients that already installed the bad release stay on it until a newer one is offered. The
  updater never goes back to an older version.

## Downloads for the bot and players

- **`https://plasmatrack-server.jansherremway.workers.dev/dl/setup`** always points to the
  newest offered setup program (a `302` to the file). It is the link to share.
- **`GET /api/v1/public/client`:** what the bot shows. It returns the version, notes, and the
  setup program's name, size and that link (PUBLIC_API §7).
- **Release files** never change once uploaded, so they are served with long cache lifetimes.
  The setup program is not code-signed, so Windows SmartScreen warns on first run. Updates
  downloaded by the client do not trigger SmartScreen.
