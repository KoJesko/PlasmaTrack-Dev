'use strict';

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('setup', {
  state: () => ipcRenderer.invoke('state'),
  install: (opts) => ipcRenderer.invoke('install', opts),
  uninstall: (opts) => ipcRenderer.invoke('uninstall', opts),
  start: () => ipcRenderer.invoke('start'),
  openStatus: (url) => ipcRenderer.invoke('open-status', { url }),
});
