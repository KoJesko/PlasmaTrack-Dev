// Copies the files the installer puts on the player's PC into payload/:
// the client (console and windowless), the uninstaller, the plugin DLL and the
// licence texts.
// Build them first: `cargo build --release -p pt-client` and
// `scripts/build-plugin.ps1 -Config Release`.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const root = path.resolve(__dirname, '..', '..');
const out = path.resolve(__dirname, '..', 'payload');

const files = [
  ['target/release/plasmatrack-client.exe', 'plasmatrack-client.exe'],
  ['target/release/plasmatrack-clientw.exe', 'plasmatrack-clientw.exe'],
  ['target/release/plasmatrack-uninstall.exe', 'plasmatrack-uninstall.exe'],
  ['plugin/build-win/plasmatrack.dll', 'plasmatrack.dll'],
  ['LICENSE.md', 'LICENSE.md'],
  ['THIRD_PARTY_NOTICES.md', 'THIRD_PARTY_NOTICES.md'],
  ['third_party/scssdk-1.14/sdk_license.txt', 'licenses/scssdk-1.14-LICENSE.txt'],
  ['third_party/truckersmp-client-sdk-1.1.0/LICENSE', 'licenses/truckersmp-client-sdk-LICENSE.txt'],
  ['third_party/notices/LLVM-LICENSE.txt', 'licenses/LLVM-LICENSE.txt'],
  ['third_party/notices/COPYING.MinGW-w64-runtime.txt', 'licenses/COPYING.MinGW-w64-runtime.txt'],
  ['third_party/notices/winpthreads-COPYING.txt', 'licenses/winpthreads-COPYING.txt'],
];

fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(path.join(out, 'licenses'), { recursive: true });
const manifest = {};
for (const [from, to] of files) {
  const src = path.join(root, from);
  if (!fs.existsSync(src)) {
    console.error(`missing ${from}; build it first (see installer/README.md)`);
    process.exit(1);
  }
  fs.copyFileSync(src, path.join(out, to));
  manifest[to] = crypto.createHash('sha256').update(fs.readFileSync(src)).digest('hex');
  console.log(`payload/${to}  ${manifest[to].slice(0, 16)}`);
}
fs.writeFileSync(path.join(out, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
