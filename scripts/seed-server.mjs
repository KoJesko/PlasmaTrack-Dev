#!/usr/bin/env node
// Seeds a PlasmaTrack server with recorded captures, through the real HTTP API
// exactly as the client and the bot would use it. For local checks and for the
// test instance; never point it at production with --publish.
//
//   node scripts/seed-server.mjs --url http://127.0.0.1:8787 --bot-token T --actor 900000000000000001 \
//        [--publish] [--player 123456789012345678 --name "Test Driver"] fixture.jsonl.gz ...
//
// --publish  publishes config/plasmatrack.sample.toml and a test release whose
//            hashes match the manifest this script uploads (test data only).
// Prints the player's token once (so later runs can reuse it with --token).

import { readFileSync } from "node:fs";
import { gunzipSync, gzipSync } from "node:zlib";
import { createHash, randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(`--${name}`);
  if (i < 0) return dflt;
  const v = args[i + 1];
  args.splice(i, 2);
  return v;
};
const flag = (name) => {
  const i = args.indexOf(`--${name}`);
  if (i < 0) return false;
  args.splice(i, 1);
  return true;
};
const url = opt("url", "http://127.0.0.1:8787").replace(/\/$/, "");
const botToken = opt("bot-token");
const actor = opt("actor");
const player = opt("player", "123456789012345678");
const name = opt("name", "Test Driver");
let token = opt("token");
const perChunk = Number(opt("chunk", "2000"));
const publish = flag("publish");
const files = args;
if (!botToken || !actor || files.length === 0) {
  console.error("usage: seed-server.mjs --url U --bot-token T --actor DISCORD_ID [--publish] [--player ID] files...");
  process.exit(2);
}

// The manifest the fake client reports, and the release that matches it.
const PLUGIN_SHA = createHash("sha256").update("plasmatrack-test-plugin").digest("hex");
const CLIENT_SHA = createHash("sha256").update("plasmatrack-test-client").digest("hex");
const manifest = [
  { role: "plugin", path: "C:\\Steam\\steamapps\\common\\Game\\bin\\win_x64\\plugins\\plasmatrack.dll", sha256: PLUGIN_SHA },
  { role: "client_binary", path: "C:\\PlasmaTrack\\bin\\plasmatrack-clientw.exe", sha256: CLIENT_SHA },
];

async function call(method, path, body, headers = {}) {
  const res = await fetch(url + path, {
    method,
    headers: { authorization: `Bearer ${botToken}`, "content-type": "application/json", ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = text;
  }
  return { status: res.status, json };
}
const admin = (method, path, body) =>
  call(method, path, body, { "x-plasmatrack-actor": actor, "idempotency-key": randomUUID() });

function must(r, ...ok) {
  if (!ok.includes(r.status)) {
    console.error(JSON.stringify(r.json, null, 2));
    throw new Error(`HTTP ${r.status}`);
  }
  return r.json;
}

if (publish) {
  const toml = readFileSync(join(root, "config", "plasmatrack.sample.toml"), "utf8");
  const c = await admin("POST", "/api/v1/admin/config-versions", { toml, make_current: true, reason: "Seed: sample config" });
  console.log("config:", c.status === 409 ? "already published" : must(c, 201).version_id);
  const r = await admin("POST", "/api/v1/admin/releases", {
    release_version: "0.0.0-test",
    platform: "windows",
    reason: "Seed: test release matching the seed script",
    files: [
      { role: "plugin", path_pattern: "*/plugins/plasmatrack.dll", sha256: PLUGIN_SHA },
      { role: "client_binary", path_pattern: "*/plasmatrack-clientw.exe", sha256: CLIENT_SHA },
    ],
  });
  console.log("release:", r.status === 409 ? "already published" : must(r, 201).release_id);
}

if (!token) {
  const r = must(
    await call("POST", `/api/v1/players/${player}/token`, { display_name: name }, { "idempotency-key": randomUUID() }),
    201,
  );
  token = r.token.token;
  console.log(`player ${player} linked; token (shown once): ${token}`);
}

// The config hash the server expects comes from pt-core; the seed asks the
// server for nothing and simply sends what the client would: the sample config.
const configJson = JSON.parse(readFileSync(join(root, "crates", "pt-server", "tests", "sample-config.json"), "utf8"));

for (const file of files) {
  const raw = readFileSync(file);
  const text = (file.endsWith(".gz") ? gunzipSync(raw) : raw).toString("utf8");
  const sessions = new Map();
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    const v = JSON.parse(line);
    if (!sessions.has(v.sess)) sessions.set(v.sess, []);
    sessions.get(v.sess).push(line);
  }
  let chunks = 0;
  for (const [sess, lines] of sessions) {
    for (let i = 0; i < lines.length; i += perChunk) {
      const part = lines.slice(i, i + perChunk);
      const seqs = part.map((l) => JSON.parse(l).seq ?? 0).filter((s) => s > 0);
      const chunk = {
        protocol: 1,
        client_version: "0.0.0-seed",
        sess,
        hello: null,
        seq_from: seqs.length ? Math.min(...seqs) : 0,
        seq_to: seqs.length ? Math.max(...seqs) : 0,
        frames: part.map((l) => JSON.parse(l)),
        config_hash: configJson.hash,
        config: configJson.config,
        manifest,
      };
      const res = await fetch(`${url}/api/v1/chunks`, {
        method: "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json", "content-encoding": "gzip" },
        body: gzipSync(Buffer.from(JSON.stringify(chunk))),
      });
      if (res.status !== 200) throw new Error(`upload ${file}: HTTP ${res.status} ${await res.text()}`);
      chunks++;
    }
  }
  console.log(`${file}: ${sessions.size} session(s), ${chunks} chunk(s) uploaded`);
}

// Scoring runs in the background; give it a moment, then show the jobs.
await new Promise((r) => setTimeout(r, 3000));
for (const game of ["ats", "ets2"]) {
  const r = must(await call("GET", `/api/v1/public/${game}/players/${player}/jobs?limit=100`), 200);
  for (const j of r.items) {
    console.log(`${game} ${j.job_id} ${j.outcome.padEnd(14)} ${String(j.points ?? j.held_points).padStart(6)} pts  ${j.cargo.name}: ${j.source.city_name} -> ${j.destination.city_name}`);
  }
}
