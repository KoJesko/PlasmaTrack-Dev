'use strict';

const $ = (id) => document.getElementById(id);
const GAME_NAMES = { ats: 'American Truck Simulator', ets2: 'Euro Truck Simulator 2' };
let current = null;

function item(cls, text, detail) {
  const li = document.createElement('li');
  li.className = cls;
  li.textContent = text;
  if (detail) {
    const d = document.createElement('span');
    d.className = 'detail';
    d.textContent = detail;
    li.append(d);
  }
  return li;
}

function render(s) {
  current = s;
  $('version').textContent = `v${s.version}`;

  const games = s.games.filter((g) => g.windows_build);
  $('games').replaceChildren(
    ...(games.length
      ? games.map((g) =>
          item(g.plugin_installed ? 'ok' : 'todo', GAME_NAMES[g.game] || g.game, g.plugin_installed ? 'Plugin installed' : 'Plugin not installed yet'))
      : [item('bad', 'No ETS2 or ATS install found in your Steam libraries')]),
  );
  const running = s.gamesRunning;
  $('games-running').hidden = running.length === 0;
  $('games-running').textContent = 'Close the game before installing or uninstalling, so the plugin can be replaced.';

  if (!$('server').value) $('server').value = s.serverUrl;
  $('token').placeholder = s.tokenSet ? 'Saved (leave empty to keep it)' : 'Paste your token';
  if (s.installed) $('autostart').checked = s.autostart;

  $('install').textContent = s.installed ? 'Update' : 'Install';
  $('install').disabled = games.length === 0;
  $('status').hidden = !s.running;
  $('start').hidden = !s.installed || s.running;
  $('remove').hidden = !s.installed;
}

function showSteps(title, result) {
  $('result').hidden = false;
  if (result.error) {
    $('result-title').textContent = 'Something went wrong';
    $('steps').replaceChildren(item('bad', result.error));
    return;
  }
  const failed = result.steps.some((s) => !s.ok);
  $('result-title').textContent = failed ? `${title}, with problems` : title;
  $('steps').replaceChildren(...result.steps.map((s) => item(s.ok ? 'ok' : 'bad', s.text, s.detail)));
}

async function refresh() {
  const s = await window.setup.state();
  if (s.error) return showSteps('', s);
  render(s);
}

async function busy(button, label, fn) {
  const all = document.querySelectorAll('button');
  const old = button.textContent;
  all.forEach((b) => (b.disabled = true));
  button.textContent = label;
  try {
    await fn();
  } finally {
    button.textContent = old;
    all.forEach((b) => (b.disabled = false));
    await refresh();
  }
}

$('install').addEventListener('click', () =>
  busy($('install'), 'Installing…', async () => {
    const server = $('server').value.trim();
    const token = $('token').value.trim();
    if (!token && !current.tokenSet) {
      if (!confirm('No token entered. PlasmaTrack will record your jobs but not upload them until you add one. Continue?')) return;
    }
    const r = await window.setup.install({ serverUrl: server, token, autostart: $('autostart').checked });
    $('token').value = '';
    showSteps(current.installed ? 'Updated' : 'Installed', r);
  }),
);

$('uninstall').addEventListener('click', () =>
  busy($('uninstall'), 'Uninstalling…', async () => {
    const del = $('delete-data').checked;
    if (del && !confirm('Delete all local PlasmaTrack data, including anything not uploaded yet?')) return;
    showSteps('Uninstalled', await window.setup.uninstall({ deleteData: del }));
  }),
);

$('status').addEventListener('click', () => window.setup.openStatus(current.statusUrl));
$('start').addEventListener('click', () =>
  busy($('start'), 'Starting…', async () => {
    await window.setup.start();
    await new Promise((r) => setTimeout(r, 1500));
  }),
);

refresh();
