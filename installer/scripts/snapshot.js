// Dev check: starts the installer, waits for the page to load its state, saves
// a PNG of the window and quits. Only reads (games, config); never installs.
//   npx electron scripts/snapshot.js OUT.png [dark|light]
'use strict';

const { app, nativeTheme } = require('electron');
const fs = require('node:fs');

const [out, theme] = process.argv.slice(-2).filter((a) => !a.endsWith('snapshot.js'));
nativeTheme.themeSource = theme === 'dark' ? 'dark' : 'light';

app.on('browser-window-created', (_e, win) => {
  win.webContents.once('did-finish-load', () => {
    setTimeout(async () => {
      const img = await win.webContents.capturePage();
      fs.writeFileSync(out || 'snapshot.png', img.toPNG());
      app.quit();
    }, 4000);
  });
});

require('../src/main.js');
