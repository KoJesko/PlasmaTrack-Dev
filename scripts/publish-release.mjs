#!/usr/bin/env node
// Publishes the current config (config/plasmatrack.sample.toml) and the release
// in installer/payload (its manifest.json, written by `npm run payload`) to a
// server, so jobs from that build pass rule 0 (PLAN §6a).
//
//   node scripts/publish-release.mjs --env production|test --url https://… --actor DISCORD_ID [--version 0.1.0]
//
// The bot token is read from crates/pt-worker/.secrets.<env>.json (make-secrets.mjs).

import { readFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`);
  return i < 0 ? d : process.argv[i + 1];
};
const env = arg("env");
const url = (arg("url") || "").replace(/\/$/, "");
const actor = arg("actor");
const version = arg("version", /^version = "(.+)"/m.exec(readFileSync(join(root, "crates", "pt-client", "Cargo.toml"), "utf8"))[1]);
if (!env || !url || !actor) {
  console.error("usage: publish-release.mjs --env production|test --url URL --actor DISCORD_ID [--version V]");
  process.exit(2);
}
const { BOT_TOKEN } = JSON.parse(readFileSync(join(root, "crates", "pt-worker", `.secrets.${env}.json`), "utf8"));
const manifest = JSON.parse(readFileSync(join(root, "installer", "payload", "manifest.json"), "utf8"));

async function post(path, body) {
  const res = await fetch(url + path, {
    method: "POST",
    headers: {
      authorization: `Bearer ${BOT_TOKEN}`,
      "x-plasmatrack-actor": actor,
      "idempotency-key": randomUUID(),
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
  return { status: res.status, json: await res.json() };
}

const toml = readFileSync(join(root, "config", "plasmatrack.sample.toml"), "utf8");
let r = await post("/api/v1/admin/config-versions", { toml, make_current: true, reason: `Config shipped with PlasmaTrack ${version}` });
console.log("config:", r.status, r.json.version_id ?? r.json.error?.message);

const files = [
  { role: "plugin", path_pattern: "*/plugins/plasmatrack.dll", sha256: manifest["plasmatrack.dll"] },
  { role: "client_binary", path_pattern: "*/plasmatrack-clientw.exe", sha256: manifest["plasmatrack-clientw.exe"] },
  { role: "client_binary", path_pattern: "*/plasmatrack-client.exe", sha256: manifest["plasmatrack-client.exe"] },
];
r = await post("/api/v1/admin/releases", { release_version: version, platform: "windows", files, reason: `Release ${version}` });
console.log("release:", r.status, r.json.release_id ?? r.json.error?.message);
