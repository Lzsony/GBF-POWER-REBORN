import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const cases = JSON.parse(readFileSync(new URL('../tests/fixtures/target-rules.json', import.meta.url)));
for (const port of [8123, 18123]) {
  const r = spawnSync('cargo', ['run', '--quiet', '-p', 'gbf-core', '--example', 'export_proxy_rules', '--locked', '--', String(port)], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  const { pac } = JSON.parse(r.stdout);
  const context = vm.createContext({}); vm.runInContext(pac, context);
  for (const c of cases) assert.equal(context.FindProxyForURL(`https://${c.host}/`, c.host), c.target ? `PROXY 127.0.0.1:${port}` : 'DIRECT', c.host);
  assert.ok(!pac.includes('; DIRECT'));
}
console.log(`PAC: ${cases.length} shared cases passed on two ports`);
