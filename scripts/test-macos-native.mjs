// Real WKWebView, production React and IPC; no screenshots or browser emulation.
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import net from 'node:net';
import assert from 'node:assert/strict';

if (process.platform !== 'darwin' || !process.argv[2]) throw new Error('Usage: node scripts/test-macos-native.mjs <internal-test binary> [summary.json]');
const exe = path.resolve(process.argv[2]);
if (!(await readFile(exe)).includes(Buffer.from('GBF-INTERNAL-TEST-BUILD-DO-NOT-DISTRIBUTE'))) throw new Error('Requires internal-test build');
const local = await mkdtemp(path.join(os.tmpdir(), 'gbf-macos-'));
const data = path.join(local, 'data');
const report = path.join(local, 'report.json');
const identity = `cc.lzsony.gbf-power-reborn.test-${path.basename(local)}`;
const script = path.resolve('scripts/macos-webview-test.js');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const freePort = async () => {
  const server = net.createServer().listen(0, '127.0.0.1'); await once(server, 'listening');
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
};
const tcpServer = net.createServer(socket => socket.end());
const summaries = [];
let child;
const fixture = {};
const update = async patch => { Object.assign(fixture, patch); await writeFile(path.join(data, 'fixture.json'), JSON.stringify(fixture)); };
async function run(stage) {
  await rm(report, { force: true });
  await update({ stage, pacPassed: false });
  const env = { ...process.env, GBF_INTERNAL_TEST_ID: identity, GBF_INTERNAL_TEST_DATA: data, GBF_INTERNAL_TEST_SCRIPT: script, GBF_INTERNAL_TEST_REPORT: report };
  child = spawn(exe, [], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = ''; child.stdout.on('data', b => { output += b; }); child.stderr.on('data', b => { output += b; });
  const ended = once(child, 'exit');
  let single = false, final;
  const end = Date.now() + 90000;
  while (Date.now() < end) {
    const value = await readFile(report, 'utf8').then(JSON.parse).catch(() => null);
    if (value?.phase === 'pac' && !fixture.pacPassed) {
      const response = await fetch(`http://127.0.0.1:${value.port}/proxy.pac`);
      assert.equal(response.status, 200); assert.match(await response.text(), /FindProxyForURL/);
      if (!single) {
        const other = spawn(exe, [], { env, stdio: 'ignore' });
        const outcome = await Promise.race([once(other, 'exit'), sleep(5000).then(() => null)]);
        if (!outcome) { other.kill(); throw new Error('Second instance did not exit'); }
        assert.equal(outcome[0], 0); single = true;
      }
      await update({ pacPassed: true });
    }
    if (typeof value?.passed === 'boolean') { final = value; break; }
    if (child.exitCode !== null) throw new Error(`Application exited before report: ${output.slice(-3000)}`);
    await sleep(40);
  }
  assert.ok(final, `Native test deadline: ${await readFile(report,'utf8').catch(()=>'(no script report)')} ${output.slice(-3000)}`);
  assert.equal(final.passed, true, JSON.stringify(final));
  const exit = await Promise.race([ended, sleep(5000).then(() => null)]);
  assert.ok(exit, 'Application did not quit'); assert.equal(exit[0], 0);
  assert.match(final.userAgent, /AppleWebKit/);
  summaries.push({ stage, ...final, singleInstance: single });
  console.log(JSON.stringify(summaries.at(-1)));
}
try {
  await mkdir(data);
  tcpServer.listen(0, '127.0.0.1'); await once(tcpServer, 'listening');
  for (const [name, contents] of [['config.json', '{"schemaVersion":2}'], ['config.json', 'broken']]) {
    await rm(report, { force: true });
    const file = path.join(data, name);
    await writeFile(file, contents);
    child = spawn(exe, [], { stdio: ['ignore', 'ignore', 'pipe'], env: { ...process.env, GBF_INTERNAL_TEST_ID: identity, GBF_INTERNAL_TEST_DATA: data, GBF_INTERNAL_TEST_REPORT: report, GBF_INTERNAL_TEST_STARTUP_ERROR: '1' } });
    let startupError = ''; child.stderr.on('data', bytes => { startupError += bytes; });
    const exit = await Promise.race([once(child, 'exit'), sleep(10000).then(() => null)]);
    assert.ok(exit, 'Startup alert did not finish'); assert.equal(exit[0], 1, startupError);
    assert.equal(await readFile(file, 'utf8'), contents, 'Startup failure modified original data');
    assert.equal(JSON.parse(await readFile(report, 'utf8')).passed, true, 'Native startup alert was not visible');
    await rm(file);
  }
  summaries.push({ stage: 'startup-errors', passed: true, checks: ['visible-native-alert', 'nonzero-exit', 'unsupported-malformed-config-preserved'] });
  const port = await freePort();
  await update({ port, proxyTestPort: tcpServer.address().port });
  await writeFile(path.join(data, 'config.json'), JSON.stringify({ schemaVersion: 1, settings: { listenPort: await freePort() } }));
  await run('fresh');
  const saved = JSON.parse(await readFile(path.join(data, 'config.json'), 'utf8'));
  assert.equal(saved.schemaVersion, 1); assert.equal(saved.settings.listenPort, port);
  assert.deepEqual(saved.settings.cachePreferences, {prefetchEnabled:false, warmupEnabled:false});
  await run('restart');
  if (process.argv[3]) await writeFile(process.argv[3], JSON.stringify({ passed: true, summaries }, null, 2));
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { const exited = once(child, 'exit'); child.kill(); await exited; }
  await new Promise(resolve => tcpServer.close(resolve));
  // Exact test identities only. Credentials are normally absent in these scenarios.
  for (const account of ['upstream', 'local-ca']) {
    try { execFileSync('/usr/bin/security', ['delete-generic-password', '-s', identity, '-a', account], { stdio: 'ignore' }); } catch {}
  }
  await rm(path.join(os.homedir(), 'Library/LaunchAgents', `${identity}.plist`), { force: true });
  assert.ok(path.basename(local).startsWith('gbf-macos-'));
  await rm(local, { recursive: true, force: true });
}
