#!/usr/bin/env node
// Publishes a release to a server (docs/UPDATES.md):
//
// 1. the current config (config/plasmatrack.sample.toml) as the current config
//    version;
// 2. the release in installer/payload (its manifest.json, written by
//    `npm run payload`), so jobs from that build pass rule 0 (PLAN §6a), with a
//    signed update manifest so older clients update themselves, and the setup
//    program installer/dist/PlasmaTrack-Setup-<version>.exe for new players.
//
//   node scripts/publish-release.mjs --env production|test --url https://… --actor DISCORD_ID
//        [--version 0.2.0] [--upload] [--notes "What changed"] [--no-offer]
//
// --upload  gzips the payload files and puts them, and the setup program, in the
//           R2 bucket plasmatrack-releases (shared by both servers, so once is
//           enough) with wrangler.
// --no-offer publishes without offering the update yet
//           (POST /api/v1/admin/releases/{id}/offer starts it later).
//
// The update manifest is signed with .release-signing-key.pem
// (scripts/make-release-key.mjs). It and the .gz files are kept in
// installer/dist/update-<version>/, so every server gets the same bytes.
// The bot token is read from crates/pt-worker/.secrets.<env>.json (make-secrets.mjs).

import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { createHash, createPrivateKey, randomUUID, sign } from "node:crypto";
import { execFileSync } from "node:child_process";
import { gzipSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`);
  return i < 0 ? d : process.argv[i + 1];
};
const flag = (n) => process.argv.includes(`--${n}`);
const env = arg("env");
const url = (arg("url") || "").replace(/\/$/, "");
const actor = arg("actor");
const version = arg("version", /^version = "(.+)"/m.exec(readFileSync(join(root, "crates", "pt-client", "Cargo.toml"), "utf8"))[1]);
const notes = arg("notes");
if (!env || !url || !actor) {
  console.error("usage: publish-release.mjs --env production|test --url URL --actor DISCORD_ID [--version V] [--upload] [--notes TEXT] [--no-offer]");
  process.exit(2);
}
const { BOT_TOKEN } = JSON.parse(readFileSync(join(root, "crates", "pt-worker", `.secrets.${env}.json`), "utf8"));
const payloadDir = join(root, "installer", "payload");
const payload = JSON.parse(readFileSync(join(payloadDir, "manifest.json"), "utf8"));
const setupName = `PlasmaTrack-Setup-${version}.exe`;
const setupPath = join(root, "installer", "dist", setupName);
const BUCKET = "plasmatrack-releases";
const PLATFORM = "windows";
const CONTEXT = Buffer.from("plasmatrack-update-v1\n");

// ---- the signed update manifest -------------------------------------------------
const outDir = join(root, "installer", "dist", `update-${version}`);
const manifestFile = join(outDir, "update.json");
const sigFile = join(outDir, "update.sig");
const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const gzPath = (p) => join(outDir, "files", ...`${p}.gz`.split("/"));
if (!existsSync(manifestFile)) {
  const files = [];
  for (const [path, hash] of Object.entries(payload)) {
    const data = readFileSync(join(payloadDir, ...path.split("/")));
    if (sha256(data) !== hash) throw new Error(`payload/${path} does not match payload/manifest.json; run npm run payload`);
    const gz = gzipSync(data, { level: 9 });
    mkdirSync(dirname(gzPath(path)), { recursive: true });
    writeFileSync(gzPath(path), gz);
    files.push({ path, sha256: hash, size: data.length, gz_size: gz.length });
  }
  const text = JSON.stringify({ product: "plasmatrack", platform: PLATFORM, version, files });
  const key = createPrivateKey(readFileSync(join(root, ".release-signing-key.pem"), "utf8"));
  const signature = sign(null, Buffer.concat([CONTEXT, Buffer.from(text)]), key).toString("hex");
  writeFileSync(manifestFile, text);
  writeFileSync(sigFile, signature);
  console.log(`signed the update manifest for ${version} (${files.length} files)`);
}
const manifestText = readFileSync(manifestFile, "utf8");
const signature = readFileSync(sigFile, "utf8").trim();
const manifest = JSON.parse(manifestText);
if (manifest.version !== version) throw new Error(`${manifestFile} is for ${manifest.version}`);

// ---- upload to R2 ---------------------------------------------------------------
if (flag("upload")) {
  const workerDir = join(root, "crates", "pt-worker");
  const childEnv = { ...process.env };
  delete childEnv.ELECTRON_RUN_AS_NODE;
  const put = (key, file, type) => {
    execFileSync("npx", ["wrangler", "r2", "object", "put", `${BUCKET}/${key}`, "--file", file, "--content-type", type, "--remote"], {
      cwd: workerDir, env: childEnv, stdio: ["ignore", "ignore", "inherit"], shell: process.platform === "win32",
    });
    console.log(`uploaded ${key} (${statSync(file).size} bytes)`);
  };
  for (const f of manifest.files) put(`${PLATFORM}/${version}/${f.path}.gz`, gzPath(f.path), "application/gzip");
  if (existsSync(setupPath)) put(`${PLATFORM}/${version}/${setupName}`, setupPath, "application/vnd.microsoft.portable-executable");
  else console.warn(`no ${setupPath}; publishing without a setup program`);
}

// ---- the server ----------------------------------------------------------------
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
console.log("config:", r.status, r.json.version_id ?? r.json.error?.message ?? "", r.json.error?.details ? JSON.stringify(r.json.error.details) : "");

const files = [
  { role: "plugin", path_pattern: "*/plugins/plasmatrack.dll", sha256: payload["plasmatrack.dll"] },
  { role: "client_binary", path_pattern: "*/plasmatrack-clientw.exe", sha256: payload["plasmatrack-clientw.exe"] },
  { role: "client_binary", path_pattern: "*/plasmatrack-client.exe", sha256: payload["plasmatrack-client.exe"] },
];
r = await post("/api/v1/admin/releases", {
  release_version: version,
  platform: PLATFORM,
  files,
  update: { manifest: manifestText, signature },
  installer: existsSync(setupPath) ? setupName : undefined,
  notes,
  offer: !flag("no-offer"),
  reason: `Release ${version}`,
});
console.log("release:", r.status, r.json.release_id ?? r.json.error?.message, r.json.error?.details ? JSON.stringify(r.json.error.details) : "");
if (r.status >= 300) process.exit(1);
