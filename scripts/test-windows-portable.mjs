// Native WebView2 integration test. Isolated data and identity; no screenshots/trace.
import { chromium, expect } from '@playwright/test';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { access, rm, readFile, mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';

if (process.platform !== 'win32' || !process.argv[2]) throw new Error('Usage: node scripts/test-windows-portable.mjs <extracted exe>');
const executables = process.argv.slice(2).filter(arg => arg !== '--isolated').map(exe => path.resolve(exe));
// --isolated is for the explicit internal-test build, never a distributed EXE.
const isolated = process.argv.includes('--isolated');
if (!isolated) throw new Error('This runner requires --isolated and an internal-test build.');
if (isolated) {
  for (const exe of executables) {
    if (!(await readFile(exe)).includes(Buffer.from('GBF-INTERNAL-TEST-BUILD-DO-NOT-DISTRIBUTE'))) {
      throw new Error('--isolated requires an internal-test build; refusing to launch a production EXE.');
    }
  }
}
// Exercise only the internal build's fixed test registration; never touch the product value.
const startupPaths = [String.raw`HKCU:\Software\Microsoft\Windows\CurrentVersion\Run`, String.raw`HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run`];
const startup = script => execFileSync('powershell.exe', ['-NoProfile','-NonInteractive','-Command', script], {windowsHide:true,encoding:'utf8'}).trim();
if (isolated) for (const key of startupPaths) {
  if (startup(`if ((Get-ItemProperty -LiteralPath '${key}' -Name 'GBF Internal Test' -ErrorAction SilentlyContinue)) { 'present' }`)) throw new Error('Existing internal test startup entry; refusing to overwrite it.');
}
const local = isolated ? await mkdtemp(path.join(os.tmpdir(), 'gbf-product-test-')) : path.resolve(process.env.LOCALAPPDATA);
const data = path.join(local, 'GBF Power Reborn');
if (path.dirname(data) !== local || path.basename(data) !== 'GBF Power Reborn') throw new Error('Unsafe test data path');
for (const exe of executables) await access(exe);
for (const name of ['GBF Power Reborn']) {
  try { await access(path.join(local, name)); throw new Error('Existing application data found; use a fresh Windows test account. Nothing changed.'); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
}
try {
for (const [index, exe] of executables.entries()) {
if (index === 0) {
  const listener = net.createServer().listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const proxyPort = listener.address().port;
  await new Promise(resolve => listener.close(resolve));
  await mkdir(data, {recursive:true});
  await writeFile(path.join(data, 'config.json'), JSON.stringify({schemaVersion:1, settings:{listenPort:proxyPort}}));
}
const manifest = await readFile(path.join(path.dirname(exe), 'manifest.json'), 'utf8').then(s=>JSON.parse(s.replace(/^\uFEFF/,''))).catch(error=> { if (error.code === 'ENOENT') return {package:'lightweight'}; throw error; });
const reservation = net.createServer();
reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(resolve => reservation.close(resolve));
const endpoint = `http://127.0.0.1:${port}`;
const childEnv = {...process.env,
  ...(isolated ? {GBF_INTERNAL_TEST_LOCALAPPDATA:local, GBF_INTERNAL_TEST_ID:`cc.lzsony.gbf-power-reborn.test-${path.basename(local)}`} : {}),
  WEBVIEW2_BROWSER_EXECUTABLE_FOLDER:path.join(os.tmpdir(),'intentionally-missing-runtime'),
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`};
const child = spawn(exe, [], {cwd:os.tmpdir(), windowsHide:true, stdio:['ignore','pipe','pipe'], env:childEnv});
let output = '';
child.stdout.on('data', chunk => { output = (output + chunk).slice(-8000); });
child.stderr.on('data', chunk => { output = (output + chunk).slice(-8000); });
const ended = once(child, 'exit');
let browser, page;
const invoke = (command, args = {}) => page.evaluate(({command,args}) => window.__TAURI_INTERNALS__.invoke(command,args), {command,args});
try {
  await expect.poll(async () => {
    if (child.exitCode !== null) throw new Error(`Product exited early: ${child.exitCode}: ${output}`);
    return fetch(`${endpoint}/json/version`, {signal:AbortSignal.timeout(1000)}).then(r => r.ok).catch(() => false);
  }, {timeout:30000}).toBe(true);
  browser = await chromium.connectOverCDP(endpoint);
  await expect.poll(() => browser.contexts().flatMap(c => c.pages()).length).toBeGreaterThan(0);
  page = browser.contexts().flatMap(c => c.pages())[0];
  const errors = []; page.on('pageerror', error => errors.push(error));
  await expect(page.locator('.start-button')).toBeEnabled({timeout:15000});
  const initial = await invoke('get_status');
  expect(await page.locator('.traffic strong').textContent()).toBe('↓ —↑ —');
  expect(await page.locator('.network-quality strong').first().evaluate(e => getComputedStyle(e).fontSize)).toBe('14px');

  expect(initial.running).toBe(false); expect(initial.settings.mode).toBe('direct');
  await expect(page.locator('.mode-field option[value="accelerate"]')).toBeDisabled();
  if (index > 0) {
    expect(initial.preferences.language).toBe('zh-TW');
    expect(initial.cachePreferences).toEqual({prefetchEnabled:false, warmupEnabled:false});
  }
  await invoke('save_cache_preferences', {patch:{prefetchEnabled:false}});
  await invoke('save_cache_preferences', {patch:{warmupEnabled:false}});
  expect((await invoke('get_status')).cachePreferences).toEqual({prefetchEnabled:false, warmupEnabled:false});
  const control = action => invoke('internal_test_control', {action});
  await control('audit-fixture');
  await invoke('start_cache_audit');
  await expect.poll(async () => (await invoke('get_status')).audit).toMatchObject({running:false,repaired:2,failed:0});
  expect((await control('snapshot')).auditFixture).toEqual({orphanExists:false,pendingExists:false,notesPreserved:true});
  await control('audit-hold');
  await invoke('start_cache_audit');
  expect((await invoke('get_native_control')).state.maintenance).toBe(true);
  const blocked = await invoke('clear_cache').then(() => null, error => error.code);
  expect(blocked).toBe('CACHE_MAINTENANCE_BUSY');
  await invoke('cancel_cache_audit');
  await control('audit-release');
  expect((await invoke('get_status')).audit).toMatchObject({running:false,cancelled:true});
  expect((await invoke('get_native_control')).state.maintenance).toBe(false);
  if (isolated) {
    if (index === 0) {
      const {hasAuthentication, ...settings} = initial.settings;
      await invoke('save_settings', {input:{...settings,proxyUrl:null,autostart:true}});
    } else expect(initial.settings.autostart).toBe(true);
    const registered = startup(`(Get-ItemProperty -LiteralPath '${startupPaths[0]}' -Name 'GBF Internal Test').'GBF Internal Test'`);
    expect(registered).toBe(`"${exe}"`);
  }

  await page.locator('.menu-trigger').click();
  for (const [language, name] of [['zh-CN','简体'],['ja','日本語'],['en','English'],['zh-TW','繁體']]) {
    await page.getByRole('menuitemradio',{name,exact:true}).click();
    await expect(page.locator('html')).toHaveAttribute('lang',language);
    await expect.poll(async () => (await invoke('get_status')).preferences.language).toBe(language);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
  await page.keyboard.press('Escape');
  await expect(page.getByLabel('模式',{exact:true})).toHaveValue('direct');
  await page.locator('.start-button').click();
  await expect(page.locator('.start-button')).toHaveClass(/is-running/);
  const running = await invoke('get_status');
  const pac = await fetch(running.pacUrl).then(r => r.text());
  expect(pac).toContain('FindProxyForURL'); expect(pac).toContain(`PROXY 127.0.0.1:${running.settings.listenPort}`);
  expect(pac).not.toContain('; DIRECT');
  if (executables.length > 1) {
    const secondary = spawn(executables[(index+1)%executables.length], [], {cwd:os.tmpdir(), windowsHide:true, stdio:'ignore', env:childEnv});
    let timer;
    try {
      const result = await Promise.race([once(secondary,'exit').then(([code])=>code),
        new Promise((_,reject)=> { timer=setTimeout(()=>reject(new Error('Second instance did not exit')),10000); })]);
      expect(result).toBe(0);
      expect((await invoke('get_status')).running).toBe(true);
    } finally {
      clearTimeout(timer);
      if (secondary.exitCode === null) secondary.kill();
    }
  }
  await page.locator('.start-button').click();
  await expect(page.locator('.start-button')).not.toHaveClass(/is-running/);
  expect((await invoke('get_status')).running).toBe(false);
  await expect.poll(() => fetch(running.pacUrl,{signal:AbortSignal.timeout(1000)}).then(()=>false).catch(()=>true)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const session = await browser.newBrowserCDPSession();
  const version = await session.send('Browser.getVersion');
  const processes = await session.send('SystemInfo.getProcessInfo');
  const pid = processes.processInfo.find(p => p.type === 'browser').id;
  if (!Number.isSafeInteger(pid)) throw new Error('Invalid WebView2 process id');
  const runtimeExe = execFileSync('powershell.exe',['-NoProfile','-NonInteractive','-Command',`(Get-Process -Id ${pid}).Path`],{windowsHide:true,encoding:'utf8'}).trim();
  const bundled = path.join(path.dirname(exe),'WebView2Runtime').toLowerCase();
  if (manifest.package === 'portable_webview2') expect(path.dirname(runtimeExe).toLowerCase()).toBe(bundled);
  else expect(path.dirname(runtimeExe).toLowerCase()).not.toBe(bundled);
  await access(path.join(data,'runtime/EBWebView'));
  const config = JSON.parse(await readFile(path.join(data, 'config.json'), 'utf8'));
  expect(config.schemaVersion).toBe(1);
  expect(config.settings.preferences.language).toBe('zh-TW');
  expect(config.settings.cachePreferences).toEqual({prefetchEnabled:false,warmupEnabled:false});
  expect(errors).toHaveLength(0);
  console.log(JSON.stringify({result:'PASS',package:manifest.package,dataDirectory:data,runtime:version.product,runtimeExecutable:runtimeExe,
    checks:['runtime selection overrides stale environment','current data root and WebView data','cross-package single instance preserves running proxy','relaunch preserves settings: '+(index>0),'quoted autostart tracks moved executable: '+isolated,'Chinese and spaced executable path','different working directory',
      'real WebView2 UI and IPC','cache preference persistence','audit repair/cancel/maintenance unlock','preference save','proxy start/PAC/stop/port release','no horizontal overflow','no page errors']},null,2));
  await invoke('quit_app').catch(() => {});
  await Promise.race([ended,new Promise((_,reject)=>setTimeout(()=>reject(new Error('Product did not quit')),10000))]);
} finally {
  if (child.exitCode === null) {
    if (page) await invoke('quit_app').catch(() => {});
    await Promise.race([ended,new Promise(resolve => setTimeout(resolve,3000))]);
    if (child.exitCode === null) { child.kill(); await ended; }
  }
  if (browser) await browser.close().catch(() => {});
}
}
} finally {
  if (isolated) for (const key of startupPaths) startup(`Remove-ItemProperty -LiteralPath '${key}' -Name 'GBF Internal Test' -ErrorAction SilentlyContinue`);
  // The exact root was checked absent before the first launch.
  await rm(data,{recursive:true,force:true,maxRetries:15,retryDelay:300});
  if (isolated) await rm(local,{recursive:true,force:true,maxRetries:15,retryDelay:300});
}
