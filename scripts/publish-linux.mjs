#!/usr/bin/env node
// Publishes the Linux package made by scripts/package-linux.sh to a server
// (docs/UPDATES.md, "Linux"):
//
// 1. the package's config, as the current config version only if the server does
//    not have it yet;
// 2. the release: the client's and plugin's hashes (rule 0, PLAN §6a) and the
//    package as its installer, downloaded through personal links the bot asks
//    for. Linux has no self-update, so there is no update and nothing offered.
//
//   node scripts/publish-linux.mjs --env production|test --url https://… --actor DISCORD_ID
//        [--version 0.2.6] [--upload] [--notes "What changed"]
//
// --upload puts the package in the R2 bucket plasmatrack-releases (shared by
// both servers, so once is enough). Running it again after a publish only
// checks the download.

import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
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
const notes = arg("notes");
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};
if (!env || !url || !actor) fail("usage: publish-linux.mjs --env production|test --url URL --actor DISCORD_ID [--version V] [--upload] [--notes TEXT]");
const { BOT_TOKEN } = JSON.parse(readFileSync(join(root, "crates", "pt-worker", `.secrets.${env}.json`), "utf8"));

const folder = `PlasmaTrack-${version}-linux-x86_64`;
const name = `${folder}.tar.gz`;
const tarball = join(root, "installer", "dist", `linux-${version}`, name);
if (!existsSync(tarball)) fail(`no ${tarball}; run scripts/package-linux.sh first`);
const sha256 = (b) => createHash("sha256").update(b).digest("hex");

// ---- what the package holds ----------------------------------------------------
// Hashes come from the package itself, never from a build folder.
const unpacked = mkdtempSync(join(tmpdir(), "pt-linux-"));
let files, toml;
try {
  execFileSync("tar", ["-xzf", tarball, "-C", unpacked]);
  const inside = (f) => join(unpacked, folder, f);
  const entry = (role, path_pattern, f) => ({ role, path_pattern, sha256: sha256(readFileSync(inside(f))) });
  files = [
    entry("client_binary", "*/plasmatrack-client", "plasmatrack-client"),
    entry("plugin", "*/plugins/plasmatrack.dll", "plasmatrack.dll"),
    ...(existsSync(inside("plasmatrack.so")) ? [entry("plugin", "*/plugins/plasmatrack.so", "plasmatrack.so")] : []),
  ];
  toml = readFileSync(inside("plasmatrack.sample.toml"), "utf8");
} finally {
  rmSync(unpacked, { recursive: true, force: true });
}

// ---- the server ----------------------------------------------------------------
const auth = { authorization: `Bearer ${BOT_TOKEN}`, "x-plasmatrack-actor": actor };
async function get(path) {
  const res = await fetch(url + path, { headers: auth });
  return { status: res.status, json: await res.json().catch(() => ({})) };
}
async function post(path, body) {
  const res = await fetch(url + path, {
    method: "POST",
    headers: { ...auth, "idempotency-key": randomUUID(), "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  return { status: res.status, json: await res.json().catch(() => ({})) };
}
const problem = (r) => `${r.status} ${r.json.error?.message ?? ""} ${r.json.error?.details ? JSON.stringify(r.json.error.details) : ""}`;

// Look before uploading: a published release must keep the bytes it lists.
const releaseId = `rel_${version.replace(/[.-]/g, "_")}_linux`;
const list = await get("/api/v1/admin/releases?limit=100");
if (list.status !== 200) fail(`cannot list releases: ${problem(list)}`);
const published = list.json.items.find((r) => r.release_id === releaseId);
const key = (f) => `${f.role} ${f.path_pattern} ${f.sha256}`;
if (published) {
  const same = published.installer === name && published.files.map(key).sort().join() === files.map(key).sort().join();
  if (!same) {
    fail(`${releaseId} is already published with other files or installer ${published.installer ?? "(none)"}.\nA release cannot be changed: bump the version in crates/pt-client/Cargo.toml and build again.`);
  }
}

// ---- upload to R2 --------------------------------------------------------------
if (process.argv.includes("--upload")) {
  const workerDir = join(root, "crates", "pt-worker");
  const childEnv = { ...process.env };
  delete childEnv.ELECTRON_RUN_AS_NODE;
  // The wrangler that crates/pt-worker installs (npm ci there), run by this node.
  const wrangler = join(workerDir, "node_modules", "wrangler", "bin", "wrangler.js");
  execFileSync(process.execPath, [wrangler, "r2", "object", "put", `plasmatrack-releases/linux/${version}/${name}`, "--file", tarball, "--content-type", "application/gzip", "--remote"], {
    cwd: workerDir, env: childEnv, stdio: ["ignore", "ignore", "inherit"],
  });
  console.log(`uploaded linux/${version}/${name} (${statSync(tarball).size} bytes)`);
}

if (published) {
  console.log(`release: ${releaseId} is already published`);
} else {
  // 409: the server has this config already, and which one is current stays as it is.
  let r = await post("/api/v1/admin/config-versions", { toml, make_current: true, reason: `Config shipped with PlasmaTrack ${version} for Linux` });
  if (r.status === 409) console.log(`config: unchanged (${r.json.error?.details?.version_id})`);
  else if (r.status < 300) console.log(`config: published ${r.json.version_id} and made it current`);
  else fail(`config: ${problem(r)}`);

  r = await post("/api/v1/admin/releases", {
    release_version: version, platform: "linux", files, installer: name, notes, offer: false, reason: `Linux release ${version}`,
  });
  if (r.status >= 300) fail(`release: ${problem(r)}`);
  console.log(`release: ${r.json.release_id}`);
}

// ---- check what a player gets --------------------------------------------------
const link = await get(`/api/v1/players/${actor}/client-download?platform=linux`);
if (link.status !== 200) fail(`no Linux download (${problem(link)}). Deploy the server code for Linux releases, then run this again.`);
if (link.json.version !== version) {
  console.log(`note: /download hands out ${link.json.version}, which is newer; ${version} was not checked`);
  process.exit(0);
}
const got = await fetch(link.json.url);
if (!got.ok) fail(`the personal link answers ${got.status}. Deploy the server code for Linux releases, then run this again.`);
if (sha256(Buffer.from(await got.arrayBuffer())) !== sha256(readFileSync(tarball))) {
  fail("the downloaded package differs from the local one; run again with --upload");
}
console.log(`checked: /download platform:linux serves ${name}`);
