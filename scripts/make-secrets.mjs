#!/usr/bin/env node
// Generates the server's secrets for one deployment and stores them in
// Cloudflare with `wrangler secret bulk`. A copy stays in
// crates/pt-worker/.secrets.<env>.json (gitignored): the bot owner copies the
// bot token, read token and webhook secret into the bot from there.
//
//   node scripts/make-secrets.mjs --env production|test --webhook-url URL --admins ID[,ID…] [--keep]
//
// --keep reuses the tokens already in the file (to change the URL or admins only).

import { existsSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const dir = join(dirname(fileURLToPath(import.meta.url)), "..", "crates", "pt-worker");
const arg = (n) => {
  const i = process.argv.indexOf(`--${n}`);
  return i < 0 ? undefined : process.argv[i + 1];
};
const env = arg("env");
const webhookUrl = arg("webhook-url");
const admins = arg("admins");
if (!["production", "test"].includes(env) || !webhookUrl || !admins) {
  console.error("usage: make-secrets.mjs --env production|test --webhook-url URL --admins ID[,ID…] [--keep]");
  process.exit(2);
}
if (!/^\d{1,20}(,\d{1,20})*$/.test(admins)) throw new Error("--admins: comma-separated Discord IDs");
const file = join(dir, `.secrets.${env}.json`);
const old = process.argv.includes("--keep") && existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
const rnd = (prefix) => prefix + randomBytes(32).toString("base64url");
const secrets = {
  READ_TOKEN: old.READ_TOKEN ?? rnd("ptr_"),
  BOT_TOKEN: old.BOT_TOKEN ?? rnd("ptb_"),
  WEBHOOK_SECRETS: old.WEBHOOK_SECRETS ?? randomBytes(32).toString("base64url"),
  DOWNLOAD_SECRET: old.DOWNLOAD_SECRET ?? randomBytes(32).toString("base64url"),
  WEBHOOK_URL: webhookUrl,
  BOOTSTRAP_ADMINS: admins,
};
writeFileSync(file, JSON.stringify(secrets, null, 2) + "\n", { mode: 0o600 });
const tmp = join(dir, `.secrets.${env}.upload.json`);
writeFileSync(tmp, JSON.stringify(secrets), { mode: 0o600 });
try {
  const args = ["wrangler", "secret", "bulk", `.secrets.${env}.upload.json`]; // relative: the path has spaces
  if (env === "test") args.push("--env", "test");
  execFileSync("npx", args, { cwd: dir, stdio: "inherit", shell: process.platform === "win32" });
} finally {
  unlinkSync(tmp);
}
console.log(`secrets set for ${env}; copy kept in ${file} (never commit or paste it)`);
