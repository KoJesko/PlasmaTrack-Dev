#!/usr/bin/env node
// Creates the Ed25519 key that signs client updates (docs/UPDATES.md). The
// private key goes to .release-signing-key.pem in the repository root
// (gitignored); the public key, printed as hex, belongs in
// pt-core/src/update.rs (UPDATE_KEYS), so every client build trusts it.
//
//   node scripts/make-release-key.mjs           create the key (refuses to overwrite)
//   node scripts/make-release-key.mjs --show    print the public key of the existing one
//
// Keep a copy of the private key somewhere safe and offline: without it, no
// update can be signed for the clients already installed, and players would
// have to run a new installer by hand.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { generateKeyPairSync, createPrivateKey, createPublicKey } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const file = join(dirname(fileURLToPath(import.meta.url)), "..", ".release-signing-key.pem");
const publicHex = (key) => createPublicKey(key).export({ type: "spki", format: "der" }).subarray(-32).toString("hex");

if (process.argv.includes("--show")) {
  console.log(publicHex(createPrivateKey(readFileSync(file, "utf8"))));
} else {
  if (existsSync(file)) {
    console.error(`${file} already exists; --show prints its public key`);
    process.exit(1);
  }
  const { privateKey } = generateKeyPairSync("ed25519");
  writeFileSync(file, privateKey.export({ type: "pkcs8", format: "pem" }), { mode: 0o600 });
  console.log(`wrote ${file}`);
  console.log(`public key: ${publicHex(privateKey)}`);
}
