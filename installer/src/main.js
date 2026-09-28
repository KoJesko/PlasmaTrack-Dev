// PlasmaTrack Setup: a thin UI over `plasmatrack-client` commands
// (docs/CLIENT.md). Everything that touches game folders or the config is done
// by the client itself; this process only copies the payload, manages the
// login entry and starts/stops the background client.
'use strict';

const { app, BrowserWindow, ipcMain, shell } = require('electron');
const { execFile, spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const PAYLOAD = app.isPackaged
  ? path.join(process.resourcesPath, 'payload')
  : path.join(__dirname, '..', 'payload');
// %LOCALAPPDATA%\PlasmaTrack is also the client's data directory (paths.rs).
const DATA_DIR = path.join(process.env.LOCALAPPDATA || app.getPath('userData'), 'PlasmaTrack');
const CONFIG_DIR = path.join(process.env.APPDATA || app.getPath('appData'), 'PlasmaTrack');
const BIN_DIR = path.join(DATA_DIR, 'bin');
const CLIENTW = path.join(BIN_DIR, 'plasmatrack-clientw.exe');
const RUN_KEY = 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run';
const RUN_VALUE = 'PlasmaTrack';
const START_MENU_LINK = path.join(app.getPath('appData'), 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'PlasmaTrack.lnk');
const GAME_PROCESSES = ['eurotrucks2.exe', 'amtrucks.exe'];

/** Runs a program without a console window. Resolves even on a non-zero exit. */
function run(file, args, input) {
  return new Promise((resolve) => {
    const child = execFile(file, args, { windowsHide: true, timeout: 60_000 }, (err, stdout, stderr) => {
      resolve({ ok: !err, code: err ? err.code : 0, stdout: String(stdout), stderr: String(stderr).trim() });
    });
    if (input !== undefined) child.stdin.end(input);
  });
}

function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

/** The client that matches this installer (always present, even before install). */
const payloadClient = () => path.join(PAYLOAD, 'plasmatrack-client.exe');

async function gamesRunning() {
  const r = await run('tasklist.exe', ['/FO', 'CSV', '/NH']);
  const text = r.stdout.toLowerCase();
  return GAME_PROCESSES.filter((p) => text.includes(`"${p}"`));
}

async function autostartEnabled() {
  return (await run('reg.exe', ['query', RUN_KEY, '/v', RUN_VALUE])).ok;
}

async function clientRunning(statusUrl) {
  try {
    const res = await fetch(new URL('status.json', statusUrl), { signal: AbortSignal.timeout(1500) });
    return res.ok;
  } catch {
    return false;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function clientwRunning() {
  const r = await run('tasklist.exe', ['/FO', 'CSV', '/NH', '/FI', 'IMAGENAME eq plasmatrack-clientw.exe']);
  return r.stdout.toLowerCase().includes('plasmatrack-clientw.exe');
}

/** Asks the client to quit (so its tray icon goes away cleanly), then makes sure. */
async function stopClient() {
  await run(payloadClient(), ['stop']);
  for (let i = 0; i < 20 && (await clientwRunning()); i++) await sleep(250);
  if (await clientwRunning()) await run('taskkill.exe', ['/F', '/IM', 'plasmatrack-clientw.exe']);
  await sleep(300); // let Windows release the exe
}

function writeStartMenuShortcut() {
  return shell.writeShortcutLink(START_MENU_LINK, 'create', {
    target: CLIENTW,
    args: 'run',
    description: 'PlasmaTrack runs in the background and records your ETS2/ATS jobs',
    icon: CLIENTW,
    iconIndex: 0,
  });
}

function startClient() {
  spawn(CLIENTW, ['run'], { detached: true, stdio: 'ignore', windowsHide: true }).unref();
}

async function state() {
  const local = parseJson((await run(payloadClient(), ['show-local'])).stdout) || {};
  const games = parseJson((await run(payloadClient(), ['find-games', '--json'])).stdout) || [];
  return {
    version: app.getVersion(),
    installed: fs.existsSync(CLIENTW),
    binDir: BIN_DIR,
    games,
    gamesRunning: await gamesRunning(),
    serverUrl: local.server_url && !local.server_url.includes('.invalid') ? local.server_url : '',
    tokenSet: !!local.token_set,
    statusUrl: local.status_url || 'http://127.0.0.1:30456/',
    autostart: await autostartEnabled(),
    running: local.status_url ? await clientRunning(local.status_url) : false,
  };
}

async function install({ serverUrl, token, autostart }) {
  const steps = [];
  const step = (ok, text, detail) => steps.push({ ok, text, detail });

  await stopClient();
  // The whole payload: binaries, plugin, licence texts, manifest.json.
  fs.rmSync(BIN_DIR, { recursive: true, force: true });
  fs.cpSync(PAYLOAD, BIN_DIR, { recursive: true });
  step(true, `Copied the client to ${BIN_DIR}`);

  const client = path.join(BIN_DIR, 'plasmatrack-client.exe');
  const local = { server_url: serverUrl || undefined, token: token || undefined };
  const setLocal = await run(client, ['set-local'], JSON.stringify(local));
  step(setLocal.ok, setLocal.ok ? 'Saved server and token' : 'Could not save server and token', setLocal.stderr);
  if (!setLocal.ok) return { steps };

  const plugin = await run(client, ['install-plugin', '--plugin', path.join(BIN_DIR, 'plasmatrack.dll')]);
  const folders = parseJson(plugin.stdout);
  if (Array.isArray(folders)) {
    for (const f of folders) {
      const game = f.game === 'ats' ? 'American Truck Simulator' : 'Euro Truck Simulator 2';
      step(!f.error, f.error ? `Plugin not installed for ${game}` : `Plugin installed for ${game}`, f.error || f.plugins_dir);
    }
  } else {
    step(false, 'Plugin not installed', plugin.stderr);
  }

  const linked = writeStartMenuShortcut();
  step(linked, linked ? 'Added PlasmaTrack to the Start menu' : 'Could not add PlasmaTrack to the Start menu');

  if (autostart) {
    const r = await run('reg.exe', ['add', RUN_KEY, '/v', RUN_VALUE, '/t', 'REG_SZ', '/d', `"${CLIENTW}" run`, '/f']);
    step(r.ok, r.ok ? 'PlasmaTrack starts when you log in' : 'Could not add PlasmaTrack to startup', r.stderr);
  } else {
    await run('reg.exe', ['delete', RUN_KEY, '/v', RUN_VALUE, '/f']);
    step(true, 'PlasmaTrack does not start at login; start it from this setup');
  }

  startClient();
  const s = await state();
  let running = false;
  for (let i = 0; i < 10 && !running; i++) {
    await new Promise((r) => setTimeout(r, 500));
    running = await clientRunning(s.statusUrl);
  }
  step(
    running,
    running ? 'PlasmaTrack is running in the background (purple P icon in the taskbar tray)' : 'PlasmaTrack did not start',
    running ? s.statusUrl : path.join(DATA_DIR, 'client.log'),
  );
  return { steps };
}

async function uninstall({ deleteData }) {
  const steps = [];
  const step = (ok, text, detail) => steps.push({ ok, text, detail });

  await stopClient();
  await run('reg.exe', ['delete', RUN_KEY, '/v', RUN_VALUE, '/f']);
  fs.rmSync(START_MENU_LINK, { force: true });
  step(true, 'Stopped the client and removed it from startup and the Start menu');

  const r = await run(payloadClient(), ['uninstall-plugin']);
  const folders = parseJson(r.stdout) || [];
  for (const f of folders.filter((f) => f.changed || f.error)) {
    step(!f.error, f.error ? `Could not remove the plugin from ${f.plugins_dir}` : `Removed the plugin from ${f.plugins_dir}`, f.error);
  }

  fs.rmSync(BIN_DIR, { recursive: true, force: true });
  step(true, `Removed ${BIN_DIR}`);
  if (deleteData) {
    fs.rmSync(DATA_DIR, { recursive: true, force: true });
    fs.rmSync(CONFIG_DIR, { recursive: true, force: true });
    step(true, 'Deleted local data, settings and token');
  } else {
    step(true, `Kept local data in ${DATA_DIR} and settings in ${CONFIG_DIR}`);
  }
  return { steps };
}

async function startNow() {
  if (!fs.existsSync(CLIENTW)) return false;
  startClient();
  return true;
}

function createWindow() {
  const win = new BrowserWindow({
    width: 620,
    height: 720,
    minWidth: 420,
    minHeight: 520,
    title: 'PlasmaTrack Setup',
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  win.webContents.on('will-navigate', (e) => e.preventDefault());
  win.loadFile(path.join(__dirname, 'index.html'));
}

// Every handler catches its own errors, so the page always gets an answer.
function handle(channel, fn) {
  ipcMain.handle(channel, async (_e, arg) => {
    try {
      return await fn(arg || {});
    } catch (err) {
      return { error: String(err && err.message ? err.message : err) };
    }
  });
}

// Unattended: `--install [--no-autostart]` keeps the saved server and token
// (a token is never taken on the command line); `--uninstall` keeps local data.
// Steps are printed as JSON; the exit code is 0 when every step succeeded.
const UNATTENDED = process.argv.includes('--install') ? 'install' : process.argv.includes('--uninstall') ? 'uninstall' : null;

async function unattended() {
  const r =
    UNATTENDED === 'install'
      ? await install({ autostart: !process.argv.includes('--no-autostart') })
      : await uninstall({ deleteData: false });
  process.stdout.write(JSON.stringify(r, null, 2) + '\n');
  app.exit(r.steps && r.steps.every((s) => s.ok) ? 0 : 1);
}

app.whenReady().then(() => {
  if (UNATTENDED) {
    unattended().catch((err) => {
      process.stderr.write(`${err && err.stack ? err.stack : err}\n`);
      app.exit(1);
    });
    return;
  }
  handle('state', state);
  handle('install', install);
  handle('uninstall', uninstall);
  handle('start', startNow);
  handle('open-status', async ({ url }) => {
    // Only the local status page.
    if (/^http:\/\/127\.0\.0\.1:\d+\/$/.test(url)) await shell.openExternal(url);
  });
  createWindow();
});

app.on('window-all-closed', () => app.quit());
