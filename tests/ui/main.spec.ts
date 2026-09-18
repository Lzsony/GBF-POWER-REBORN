import { test, expect, type Page } from '@playwright/test';
import { initialStatus } from '../../src/types';
import { dictionaries, languageFromSystem, translate } from '../../src/i18n';
import { visibilityPoll } from '../../src/polling';

const languageNames = { 'zh-CN': '简体', 'zh-TW': '繁體', ja: '日本語', en: 'English' } as const;

type Options = {
  direct?: boolean; untrusted?: boolean; missingSamples?: boolean;
  configured?: boolean; authorized?: boolean;
  revealDelay?: number; revealFailure?: boolean; installFailure?: boolean;
  holdPreferences?: boolean; preferencesFailure?: boolean;
  failTest?: boolean; holdProxyTest?: boolean; proxySaveFailure?: boolean;
  switchFailure?: boolean; restoreFailure?: boolean;
  cacheDelay?: number; cacheFailure?: boolean; holdAudit?: boolean; startAuditFailure?: boolean; cancelAuditFailure?: boolean; auditFailures?: number;
};
async function desktop(page: Page, options: Options = {}) {
  await page.addInitScript(({ initial, options }) => {
    const state = structuredClone(initial);
    let savedUrl = 'socks5://user:fixture-password@127.0.0.1:7890';
    let holdPreferences = options.holdPreferences;
    state.settings.mode = options.direct ? 'direct' : 'proxy';
    state.settings.proxyUrl = 'socks5://user:••••@127.0.0.1:7890';
    state.settings.hasAuthentication = true;
    state.authorization.configured = !!options.configured;
    state.authorization.state = options.authorized ? 'active' : 'unactivated';
    if (options.authorized) state.accelerationLines = [{id:'fixture-east',name:'自建節點 A',revision:'r1'},{id:'fixture-west',name:'自建節點 B',revision:'r1'}];
    const savedSettings = sessionStorage.getItem('test-settings');
    if (savedSettings) state.settings = JSON.parse(savedSettings);
    const savedCachePreferences = sessionStorage.getItem('test-cache-preferences');
    if (savedCachePreferences) state.cachePreferences = JSON.parse(savedCachePreferences);
    const savedPreferences = sessionStorage.getItem('test-preferences');
    if (savedPreferences) state.preferences = JSON.parse(savedPreferences);
    if (!options.missingSamples) Object.assign(state.metrics.network, {
      gameMinMs: 20, gameMedianMs: 36, gameMaxMs: 70, gameLatencyMs: 36, gameJitterMs: 2.5, gameTimeoutPercent: 0,
      steamMinMs: 30, steamMedianMs: 49, steamMaxMs: 90, steamLatencyMs: 49, steamJitterMs: 3.5, steamTimeoutPercent: 10,
    });
    if (options.untrusted) state.settings.httpsCache = true;
    const commands: { command: string; args: Record<string, unknown> }[] = [];
    const callbacks = new Map<number, (event: unknown) => void>();
    const events = new Map<number, string>();
    let callbackId = 0;
    Object.assign(window, {
      isTauri: true, __commands: commands,
      __finishAudit: (failed = 0) => { Object.assign(state.audit, { running: false, cancelled: false, checked: 8, repaired: 2, failed }); },
      __auditProgress: (checked: number, repaired: number) => { Object.assign(state.audit, { checked, repaired }); },
      __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: (_event: string, id: number) => callbacks.delete(id) },
      __emitVisibility: (visible: boolean) => callbacks.forEach((cb, id) => {
        if (events.get(id) === 'main-visibility') cb({ event: 'main-visibility', payload: visible });
      }),
      __emitNative: (payload: unknown) => callbacks.forEach((cb, id) => {
        if (events.get(id) === 'native-control') cb({ event: 'native-control', payload });
      }),
      __TAURI_INTERNALS__: {
        transformCallback: (cb: (event: unknown) => void) => { callbacks.set(++callbackId, cb); return callbackId; },
        unregisterCallback: (id: number) => callbacks.delete(id),
        invoke: async (command: string, args: Record<string, unknown> = {}) => {
          commands.push({ command, args });
          if (command === 'plugin:event|listen') { events.set(args.handler as number, args.event as string); return args.handler; }
          if (command === 'plugin:event|unlisten') return;
          if (command === 'main_window_visible') return true;
          if (command === 'get_native_control') return { state: { running: state.running, busy: false, maintenance: false, shuttingDown: false }, notice: null };
          if (command === 'get_status') return structuredClone(state);
          if (command === 'get_authorization_dialog') return {registered:state.authorization.state==='active',code:null};
          if (command === 'refresh_authorization') return structuredClone(state.authorization);
          if (command === 'activate_authorization') {
            if (!args.code) throw {code:'AUTH_REQUIRED'};
            state.authorization.state = 'active';
            state.accelerationLines = [{id:'fixture-east',name:'自建節點 A',revision:'r1'},{id:'fixture-west',name:'自建節點 B',revision:'r1'}];
            return;
          }
          if (command === 'unbind_authorization') {
            state.authorization.state = 'unactivated'; state.accelerationLines = []; return;
          }
          if (command === 'reveal_proxy_url') {
            if (options.revealDelay) await new Promise(r => setTimeout(r, options.revealDelay));
            if (options.revealFailure) throw { code: 'SECRET_READ_FAILED' };
            return savedUrl;
          }
          if (command === 'save_cache_preferences') {
            if (options.cacheDelay) await new Promise(resolve => setTimeout(resolve, options.cacheDelay));
            if (options.cacheFailure) throw { code: 'CONFIG_WRITE_FAILED' };
            Object.assign(state.cachePreferences, args.patch);
            sessionStorage.setItem('test-cache-preferences', JSON.stringify(state.cachePreferences));
            return structuredClone(state.cachePreferences);
          }
          if (command === 'start_cache_audit') {
            if (options.startAuditFailure) throw { code: 'CACHE_AUDIT_FAILED' };
            state.audit = { running: !!options.holdAudit, cancelled: false, checked: options.holdAudit ? 0 : 8, repaired: options.holdAudit ? 0 : 2, failed: options.auditFailures ?? 0 };
          }
          if (command === 'cancel_cache_audit') {
            if (options.cancelAuditFailure) throw { code: 'CACHE_AUDIT_FAILED' };
            state.audit.running = false; state.audit.cancelled = true;
          }
          if (command === 'save_settings') {
            const input = args.input as typeof state.settings;
            if (options.proxySaveFailure && input.proxyUrl !== null) throw { code: 'CONFIG_WRITE_FAILED' };
            if (options.switchFailure && state.running && input.mode !== state.settings.mode) {
              if (options.restoreFailure) state.running = false;
              throw { code: options.restoreFailure ? 'SWITCH_RESTORE_FAILED' : 'SWITCH_FAILED_RESTORED' };
            }
            if (input.mode === 'proxy' && input.proxyUrl !== null) savedUrl = input.proxyUrl;
            const url = new URL(savedUrl); const hasAuthentication = !!url.username;
            if (hasAuthentication) url.password = '••••';
            state.settings = { ...state.settings, ...input, hasAuthentication, proxyUrl: url.toString() };
            state.pacUrl = `http://127.0.0.1:${state.settings.listenPort}/proxy.pac`;
            sessionStorage.setItem('test-settings', JSON.stringify(state.settings));
          }
          if (command === 'save_preferences') {
            if (holdPreferences) {
              holdPreferences = false;
              await new Promise<void>(resolve => Object.assign(window, { __finishPreferencesSave: resolve }));
            }
            if (options.preferencesFailure) throw { code: 'CONFIG_WRITE_FAILED' };
            state.preferences = args.preferences as typeof state.preferences;
            sessionStorage.setItem('test-preferences', JSON.stringify(state.preferences));
          }
          if (command === 'start_proxy') {
            state.running = true;
            if (state.settings.mode === 'accelerate') {
              state.acceleration.state = 'connected'; state.acceleration.lineId = state.settings.lineSelection==='auto'?'fixture-east':state.settings.selectedLineId;
              state.metrics.network.lineLatencyMs = 42;
            }
          }
          if (command === 'stop_proxy') {state.running = false; state.acceleration.state = 'disconnected'; state.acceleration.lineId = null;}
          if (command === 'test_line' || command === 'test_auto_lines') return {testId:args.testId,lineId:command==='test_line'?args.lineId:'fixture-east',revision:'r1',medianMs:42,state:'success',completedAt:Date.now()};
          if (command === 'cancel_line_test') return;
          if (command === 'test_proxy_url') {
            if (options.holdProxyTest) await new Promise<void>(resolve => Object.assign(window, { __finishProxyTest: resolve }));
            return { testId: args.testId, connected: !options.failTest, state: options.failTest ? 'failed' : 'success' };
          }
          if (command === 'manage_certificate') {
            state.certificate = { exists: args.action !== 'remove', trusted: args.action !== 'remove' && !options.installFailure, fingerprint: null };
            if (options.installFailure) throw { code: 'CERTIFICATE_INSTALL_FAILED' };
          }
          if (command === 'clear_cache') state.cacheBytes = 0;
        },
      },
    });
  }, { initial: initialStatus, options });
}
async function commands(page: Page) {
  return page.evaluate(() => (window as unknown as { __commands: { command: string; args: Record<string, unknown> }[] }).__commands);
}
async function menu(page: Page) {
  await page.locator('.menu-trigger').click();
  return page.locator('#settings-menu-panel');
}
async function openDesktop(page: Page, options: Options = {}) {
  await desktop(page, options); await page.goto('/');
  await expect(page.locator('.start-button')).toBeEnabled();
  if (!options.direct) await expect(page.getByLabel('代理 URL')).toHaveValue(/fixture-password/);
}

test('visibility generations discard late replies and restore one polling loop', async () => {
  const pending: ((v: number) => void)[] = []; const values: number[] = [];
  const poll = visibilityPoll(() => new Promise<number>(resolve => pending.push(resolve)), v => values.push(v), () => {}, () => {}, 20);
  poll.visibility(true); expect(pending).toHaveLength(1);
  poll.visibility(false); poll.visibility(true); poll.visibility(true); expect(pending).toHaveLength(1);
  pending.shift()!(1); await new Promise(r => setTimeout(r, 10)); expect(values).toEqual([]); expect(pending).toHaveLength(1);
  pending.shift()!(2); await Promise.resolve(); expect(values).toEqual([2]);
  poll.dispose(); await new Promise(r => setTimeout(r, 30)); expect(pending).toHaveLength(0);
});

test('all four dictionaries preserve keys, placeholders and supported system locales', () => {
  const canonical = dictionaries['zh-TW'];
  for (const dictionary of Object.values(dictionaries)) {
    expect(Object.keys(dictionary).sort()).toEqual(Object.keys(canonical).sort());
    for (const key of Object.keys(canonical) as (keyof typeof canonical)[]) {
      expect(dictionary[key].trim()).not.toBe('');
      expect((dictionary[key].match(/\{\w+\}/g) ?? []).sort()).toEqual((canonical[key].match(/\{\w+\}/g) ?? []).sort());
    }
  }
  for (const locale of ['zh-TW', 'zh-HK', 'zh-MO', 'zh-Hant', 'zh_Hant_CN']) expect(languageFromSystem(locale)).toBe('zh-TW');
  for (const locale of ['zh', 'zh-CN', 'zh-SG', 'zh-Hans', 'zh_Hans_TW']) expect(languageFromSystem(locale)).toBe('zh-CN');
  for (const locale of ['ja', 'ja-JP', 'ja_JP']) expect(languageFromSystem(locale)).toBe('ja');
  for (const locale of ['en', 'en-US', 'fr', '', 'jargon']) expect(languageFromSystem(locale)).toBe('en');
});

for (const [locale, language] of [['en-US', 'en'], ['ja-JP', 'ja'], ['fr-FR', 'en']] as const) {
  test(`preview follows ${locale} and has the correct identity`, async ({ page }) => {
    await page.addInitScript(locale => Object.defineProperty(navigator, 'language', { value: locale }), locale); await page.goto('/');
    await expect(page).toHaveTitle('GBF POWER REBORN');
    await expect(page.locator('html')).toHaveAttribute('lang', language);
    await expect(page.locator('.brand')).toHaveText('GBF POWER REBORN');
    await expect(page.locator('.statusbar')).toContainText('v0.4.0');
    await expect(page.locator('.start-button')).toBeDisabled();
    await expect(page.getByRole('status')).toHaveText(dictionaries[language].stoppedNotice);
  });
}

for (const language of ['ja', 'en'] as const) {
  test(`${language} persists across reload and localizes runtime errors`, async ({ page }) => {
    await openDesktop(page, { direct: true }); await menu(page);
    await page.getByRole('menuitemradio', { name: languageNames[language], exact: true }).click();
    await expect(page.locator('html')).toHaveAttribute('lang', language);
    await page.reload(); await expect(page.locator('html')).toHaveAttribute('lang', language);
    await menu(page);
    const dictionary = dictionaries[language];
    const port = page.getByLabel(dictionary.localPort); await port.fill('1'); await port.press('Enter');
    await expect(page.getByRole('status')).toHaveText(dictionary['error.LISTEN_PORT_INVALID']);
  });
}

test('only direct and proxy can be selected by keyboard or pointer', async ({ page }) => {
  await openDesktop(page, { direct: true });
  const select = page.getByLabel('模式', { exact: true });
  await expect(select.locator('option')).toHaveText(['直連', '代理（HTTP/SOCKS）', '加速（尚未開放）']);
  await expect(select.locator('[value=accelerate]')).toHaveJSProperty('disabled', true);
  await select.selectOption('proxy'); await expect(select).toHaveValue('proxy');
  await select.focus(); await page.keyboard.press('End'); await page.keyboard.press('Enter');
  await expect(select).toHaveValue('proxy');
  expect((await commands(page)).filter(c => c.command === 'save_settings').every(c => (c.args.input as { mode: string }).mode !== 'accelerate')).toBe(true);
  await expect(page.locator('.statistics .metric').first()).toHaveText('線路延遲—');
});

test('configured client activates and selects Control-provided Gateway lines', async ({ page }) => {
  await openDesktop(page, { direct:true, configured:true });
  await menu(page);
  await page.getByRole('menuitem', {name:/授權/}).click();
  await page.getByLabel('授權碼').fill('test-access-code');
  await page.getByRole('button', {name:'激活'}).click();
  await expect(page.getByRole('dialog', {name:'授權'})).toContainText('授權有效');
  await page.getByRole('button', {name:'關閉對話框'}).click();
  const mode = page.getByLabel('模式', {exact:true});
  await expect(mode.locator('[value=accelerate]')).toBeEnabled();
  await mode.selectOption('accelerate');
  const line = page.getByLabel('線路');
  await expect(line.locator('option')).toHaveText(['自動選擇','自建節點 A','自建節點 B']);
  await page.getByRole('button', {name:'測試'}).click();
  await expect(page.getByRole('status')).toContainText('自建節點 A · 42 ms');
  await line.selectOption('fixture-west');
  await page.getByRole('button', {name:'啟動'}).click();
  await expect(page.locator('.statistics .metric').first()).toHaveText('線路延遲42 ms');
  const calls = await commands(page);
  expect(calls.some(call=>call.command==='activate_authorization')).toBe(true);
  expect(calls.some(call=>call.command==='test_auto_lines')).toBe(true);
  expect(calls.some(call=>call.command==='save_settings'&&(call.args.input as {selectedLineId?:string}).selectedLineId==='fixture-west')).toBe(true);
});

test('menu has supported language and cache controls with keyboard navigation', async ({ page }) => {
  await openDesktop(page); await page.setViewportSize({ width: 820, height: 650 });
  const root = await menu(page);
  await expect(root.locator(':scope > button')).toHaveText(['管理憑證›', '快取管理›', '開啟目錄', '複製 PAC 位址', '退出']);
  await expect(root.getByLabel('本機埠')).toHaveValue('8123');
  await expect(root.getByRole('group', { name: '語言' }).locator('button')).toHaveText(['简体', '繁體', '日本語', 'English']);
  await expect(page.getByRole('menuitem', { name: '管理憑證' })).toBeFocused();
  await page.keyboard.press('ArrowRight');
  const sub = page.locator('#certificate-menu');
  await expect(sub).toHaveAttribute('data-side', 'left');
  await expect(page.getByRole('menuitem', { name: '安裝憑證', exact: true })).toBeFocused();
  await page.keyboard.press('Escape'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('ArrowRight');
  await expect(page.locator('#cache-menu')).toBeVisible();
  await expect(page.getByLabel('上限')).toHaveValue('5');
  await expect(page.getByRole('menuitem', { name: '清理快取…' })).toBeVisible();
  await expect(page.locator('#cache-menu input[type=checkbox]')).toHaveCount(2);
  await expect(page.getByRole('menuitemcheckbox', { name: '素材預取' })).toBeChecked();
  await expect(page.getByRole('menuitemcheckbox', { name: '記憶體預熱' })).toBeChecked();
  await page.keyboard.press('Escape'); await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: '選單', exact: true })).toBeFocused();
  expect((await commands(page)).some(c => /authorization|audit|warmup|prefetch/.test(c.command))).toBe(false);
});

test('narrow submenu remains within the window and restores focus', async ({ page }) => {
  await openDesktop(page); await page.setViewportSize({ width: 380, height: 420 }); await menu(page);
  await page.getByRole('menuitem', { name: '管理憑證' }).click();
  const child = page.locator('#certificate-menu'); await expect(child).toHaveAttribute('data-side', 'inside');
  await expect(page.locator('#settings-menu-panel')).toBeHidden();
  const box = await child.boundingBox(); expect(box!.x).toBeGreaterThanOrEqual(8); expect(box!.x + box!.width).toBeLessThanOrEqual(372); expect(box!.y + box!.height).toBeLessThanOrEqual(412);
  await page.getByRole('menuitem', { name: '返回主選單' }).click();
  await expect(page.getByRole('menuitem', { name: '管理憑證' })).toBeFocused();
  await page.keyboard.press('ArrowRight'); await page.keyboard.press('ArrowLeft'); await expect(child).toHaveCount(0);
});

test('language and theme persist while preference saves preserve keyboard focus', async ({ page }) => {
  await openDesktop(page, { holdPreferences: true }); await menu(page);
  const simplified = page.getByRole('menuitemradio', { name: '简体', exact: true }); await simplified.focus(); await page.keyboard.press('Enter');
  await expect.poll(async () => (await commands(page)).filter(c => c.command === 'save_preferences').length).toBe(1);
  await expect(simplified).toHaveAttribute('aria-disabled', 'true'); await expect(simplified).toBeFocused();
  await page.evaluate(() => (window as unknown as { __finishPreferencesSave: () => void }).__finishPreferencesSave());
  await expect(simplified).toHaveAttribute('aria-disabled', 'false');
  await page.getByRole('menuitemradio', { name: '夜晚', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(page.getByRole('menuitemradio', { name: '夜晚', exact: true })).toHaveAttribute('aria-disabled', 'false');
  await page.reload(); await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN'); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});

test('failed preferences revert language and theme with a localized error', async ({ page }) => {
  await openDesktop(page, { preferencesFailure: true }); await menu(page);
  await page.getByRole('menuitemradio', { name: '简体', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-TW');
  await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW']['error.CONFIG_WRITE_FAILED']);
});

test('automatic appearance follows the system until explicitly selected', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' }); await page.goto('/');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.emulateMedia({ colorScheme: 'dark' }); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await menu(page); await page.getByRole('menuitemradio', { name: '日間', exact: true }).click();
  await page.emulateMedia({ colorScheme: 'light' }); await page.emulateMedia({ colorScheme: 'dark' });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
});

test('URL requires a successful TCP test and explicit save; blur does not save', async ({ page }) => {
  await openDesktop(page); const input = page.getByLabel('代理 URL');
  await input.fill('socks5://alice:fixture-secret@127.0.0.1:9988'); await input.press('Tab');
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
  await input.press('Enter'); await expect(page.getByRole('button', { name: '保存', exact: true })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('TCP 連線成功');
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
  await input.press('Enter'); await expect.poll(async () => (await commands(page)).filter(c => c.command === 'save_settings').length).toBe(1);
  await input.press('Tab'); await page.waitForTimeout(100); expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(1);
  await page.getByLabel('模式', { exact: true }).selectOption('direct'); await expect(input).toHaveCount(0);
  await page.getByLabel('模式', { exact: true }).selectOption('proxy'); await expect(input).toHaveValue(/alice:fixture-secret/);
});

test('failed TCP test cannot be saved', async ({ page }) => {
  await openDesktop(page, { failTest: true }); const input = page.getByLabel('代理 URL');
  await input.fill('http://localhost:8888'); await input.press('Enter');
  await expect(page.getByRole('status')).toContainText('TCP 連線失敗');
  await expect(page.getByRole('button', { name: '保存', exact: true })).toHaveCount(0);
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
});

test('editing a tested URL forces a fresh test before saving', async ({ page }) => {
  await openDesktop(page); const input = page.getByLabel('代理 URL');
  await input.fill('http://localhost:8888'); await input.press('Enter');
  await expect(page.getByRole('button', { name: '保存', exact: true })).toBeVisible();
  await input.fill('http://localhost:9999'); await expect(page.getByRole('button', { name: '測試', exact: true })).toBeVisible();
  await expect(page.locator('.start-button')).toBeDisabled();
});

test('save failure retains the tested URL and allows retry', async ({ page }) => {
  await openDesktop(page, { proxySaveFailure: true }); const input = page.getByLabel('代理 URL');
  await input.fill('http://localhost:8888'); await input.press('Enter'); await expect(page.getByRole('button', { name: '保存', exact: true })).toBeVisible();
  await input.press('Enter'); await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW']['error.CONFIG_WRITE_FAILED']);
  await expect(input).toHaveValue('http://localhost:8888'); await expect(page.getByRole('button', { name: '保存', exact: true })).toBeEnabled();
});

test('late secret reads never overwrite an edited proxy URL', async ({ page }) => {
  await desktop(page, { revealDelay: 800 }); await page.goto('/'); const input = page.getByLabel('代理 URL');
  await expect(input).toBeEnabled(); await input.fill('http://draft@localhost:7890');
  await page.waitForTimeout(1000); await expect(input).toHaveValue('http://draft@localhost:7890');
  await expect(page.locator('.start-button')).toBeDisabled();
});

test('secret read failure leaves proxy unavailable until a new URL is supplied', async ({ page }) => {
  await desktop(page, { revealFailure: true }); await page.goto('/');
  await expect(page.getByRole('status')).toContainText(dictionaries['zh-TW']['error.SECRET_READ_FAILED']);
  await expect(page.getByLabel('代理 URL')).toHaveValue(''); await expect(page.locator('.start-button')).toBeDisabled();
  await page.getByLabel('代理 URL').fill('http://localhost:7890'); await expect(page.getByRole('button', { name: '測試', exact: true })).toBeEnabled();
});

test('numeric settings validate, PAC uses the saved port, and autostart saves independently', async ({ page }) => {
  await openDesktop(page, { direct: true }); await menu(page); const port = page.getByLabel('本機埠');
  await port.fill('1'); await port.press('Enter'); await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW']['error.LISTEN_PORT_INVALID']);
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
  await port.fill('8130'); await port.press('Enter'); await expect.poll(async () => JSON.stringify(await commands(page))).toContain('8130');
  await page.getByRole('menuitem', { name: '複製 PAC 位址', exact: true }).click();
  await expect.poll(async () => JSON.stringify(await commands(page))).toContain('http://127.0.0.1:8130/proxy.pac');
  await page.keyboard.press('Escape'); await page.getByLabel('登入時啟動', { exact: true }).check();
  await expect.poll(async () => JSON.stringify(await commands(page))).toContain('"autostart":true');
});

for (const restoreFailure of [false, true]) test(`live switch error restores settings, restoration failed=${restoreFailure}`, async ({ page }) => {
  await openDesktop(page, { direct: true, switchFailure: true, restoreFailure });
  await page.locator('.start-button').click(); await expect(page.locator('.start-button')).toHaveClass(/is-running/);
  await page.getByLabel('模式', { exact: true }).selectOption('proxy');
  await expect(page.getByLabel('模式', { exact: true })).toHaveValue('direct');
  await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW'][restoreFailure ? 'error.SWITCH_RESTORE_FAILED' : 'error.SWITCH_FAILED_RESTORED']);
  if (restoreFailure) await expect(page.locator('.start-button')).not.toHaveClass(/is-running/);
  else await expect(page.locator('.start-button')).toHaveClass(/is-running/);
});

test('successful live mode switches preserve service state and stop is red', async ({ page }) => {
  await openDesktop(page, { direct: true }); await page.locator('.start-button').click();
  await expect(page.locator('.start-button')).toHaveClass(/is-running/);
  const stopColor = await page.locator('.start-button').evaluate(e => getComputedStyle(e).backgroundColor);
  expect(stopColor).toBe('rgb(183, 53, 69)');
  await page.getByLabel('模式', { exact: true }).selectOption('proxy'); await expect(page.getByLabel('代理 URL')).toHaveValue(/fixture-password/);
  await expect(page.getByRole('status')).toHaveText('代理連線中'); await expect(page.locator('.start-button')).toHaveClass(/is-running/);
  await page.locator('.start-button').click(); await expect(page.locator('.start-button')).not.toHaveClass(/is-running/);
});

test('certificate install, clear and quit commands do not open a dialog', async ({ page }) => {
  await openDesktop(page); await menu(page); await page.getByRole('menuitem', { name: '管理憑證' }).click();
  await page.getByRole('menuitem', { name: '安裝憑證', exact: true }).click(); await expect(page.getByLabel('本機快取')).toBeEnabled();
  await menu(page); await page.getByRole('menuitem', { name: '快取管理' }).click(); await page.getByRole('menuitem', { name: '清理快取…' }).click();
  await expect(page.getByRole('dialog')).not.toBeVisible(); expect((await commands(page)).filter(c => c.command === 'clear_cache')).toHaveLength(1);
  await menu(page); await page.getByRole('menuitem', { name: '退出', exact: true }).click();
  expect((await commands(page)).filter(c => c.command === 'quit_app')).toHaveLength(1);
});

test('failed certificate install refreshes trust and displays localized error', async ({ page }) => {
  await openDesktop(page, { installFailure: true }); await menu(page); await page.getByRole('menuitem', { name: '管理憑證' }).click();
  await page.getByRole('menuitem', { name: '安裝憑證', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('憑證尚未受信任');
  await menu(page); await page.getByRole('menuitem', { name: '管理憑證' }).click();
  await expect(page.locator('.menu-status')).toContainText('未信任'); await expect(page.getByRole('menuitem', { name: '開啟憑證', exact: true })).toBeEnabled();
});

test('running locks port, capacity and certificate operations while allowing cache toggle', async ({ page }) => {
  await openDesktop(page, { direct: true }); await menu(page); await page.getByRole('menuitem', { name: '管理憑證' }).click();
  await page.getByRole('menuitem', { name: '安裝憑證', exact: true }).click(); await page.locator('.start-button').click();
  await expect(page.getByLabel('本機快取')).toBeEnabled(); await page.getByLabel('本機快取').check();
  await expect.poll(async () => JSON.stringify(await commands(page))).toContain('"httpsCache":true');
  await menu(page); await expect(page.getByLabel('本機埠')).toBeDisabled();
  await page.getByRole('menuitem', { name: '快取管理' }).click(); await expect(page.getByLabel('上限')).toBeDisabled(); await expect(page.getByRole('menuitem', { name: '清理快取…' })).toBeDisabled();
  await page.keyboard.press('Escape'); await page.getByRole('menuitem', { name: '管理憑證' }).click();
  await expect(page.getByRole('menuitem', { name: '移除憑證', exact: true })).toBeDisabled(); await expect(page.getByRole('menuitem', { name: '檢查憑證', exact: true })).toBeDisabled();
});

test('native visibility pauses status polling and preserves URL drafts', async ({ page }) => {
  await openDesktop(page); await page.getByLabel('代理 URL').fill('http://draft@localhost:7890');
  await page.evaluate(() => (window as any).__emitVisibility(false));
  const count = (await commands(page)).filter(c => c.command === 'get_status').length;
  await page.waitForTimeout(1200); expect((await commands(page)).filter(c => c.command === 'get_status')).toHaveLength(count);
  await page.evaluate(() => (window as any).__emitVisibility(true));
  await expect.poll(async () => (await commands(page)).filter(c => c.command === 'get_status').length).toBe(count + 1);
  await expect(page.getByLabel('代理 URL')).toHaveValue('http://draft@localhost:7890');
});

test('native busy state disables actions and acknowledges errors once', async ({ page }) => {
  await openDesktop(page, { direct: true });
  await page.evaluate(() => (window as any).__emitNative({ state: { running: false, busy: true, maintenance: false, shuttingDown: false }, notice: { id: 1, code: 'PROXY_BIND_FAILED' } }));
  await expect(page.locator('.start-button')).toBeDisabled(); await expect(page.getByLabel('模式', { exact: true })).toBeDisabled();
  await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW']['error.PROXY_BIND_FAILED']);
  await page.evaluate(() => (window as any).__emitNative({ state: { running: false, busy: false, maintenance: false, shuttingDown: false }, notice: { id: 1, code: 'PROXY_BIND_FAILED' } }));
  await expect(page.locator('.start-button')).toBeEnabled(); expect((await commands(page)).filter(c => c.command === 'acknowledge_native_notice')).toHaveLength(1);
});

test('quit cancels a pending URL test and late results cannot offer save', async ({ page }) => {
  await openDesktop(page, { holdProxyTest: true }); await page.getByLabel('代理 URL').fill('http://localhost:7891');
  await page.getByRole('button', { name: '測試', exact: true }).click(); await expect(page.getByRole('status')).toContainText('測試中');
  await menu(page); await page.getByRole('menuitem', { name: '退出', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('正在退出');
  expect((await commands(page)).filter(c => c.command === 'cancel_proxy_test')).toHaveLength(1);
  await expect(page.getByLabel('登入時啟動', { exact: true })).toBeDisabled();
  await page.evaluate(() => (window as any).__finishProxyTest()); await expect(page.getByRole('button', { name: '保存', exact: true })).toHaveCount(0);
});

test('missing network and cache samples remain unavailable, never zero', async ({ page }) => {
  await openDesktop(page, { direct: true, missingSamples: true });
  await expect(page.locator('.network-quality strong')).toHaveText(['— / —', '— / —']);
  await expect(page.locator('.cache-summary .metric').last()).toHaveText('本機快取命中率—');
  await expect(page.locator('.traffic strong')).toHaveText('↓ —↑ —');
});

for (const language of ['zh-CN', 'zh-TW', 'ja', 'en'] as const) for (const mode of ['direct', 'proxy'] as const) for (const viewport of [{ width: 400, height: 520 }, { width: 400, height: 438 }, { width: 380, height: 420 }]) {
  test(`${language} ${mode} layout ${viewport.width}x${viewport.height}`, async ({ page }) => {
    await openDesktop(page, { direct: mode === 'direct' }); await page.setViewportSize(viewport); await menu(page);
    await page.getByRole('menuitemradio', { name: languageNames[language], exact: true }).click();
    await expect(page.locator('html')).toHaveAttribute('lang', language);
    const languageBounds = await page.locator('.language-options button').evaluateAll(buttons => buttons.map(button => {
      const box = button.getBoundingClientRect(); const range = document.createRange(); range.selectNodeContents(button); const text = range.getBoundingClientRect();
      return { fits: text.left >= box.left - 1 && text.right <= box.right + 1, right: box.right };
    }));
    expect(languageBounds).toHaveLength(4); expect(languageBounds.every(value => value.fits && value.right <= viewport.width)).toBe(true);
    await page.keyboard.press('Escape');
    const t = dictionaries[language]; await expect(page.locator('.connection-test')).toHaveCount(mode === 'proxy' ? 1 : 0);
    const cards = page.locator('.cache-summary .metric'); expect(await cards.count()).toBe(2);
    const a = await cards.nth(0).boundingBox(); const b = await cards.nth(1).boundingBox(); expect(a!.y + a!.height).toBeLessThanOrEqual(b!.y);
    await expect(page.getByRole('link', { name: 'JP', exact: true })).toHaveAttribute('href', 'https://game.granbluefantasy.jp/');
    await expect(page.getByRole('link', { name: 'Steam', exact: true })).toHaveAttribute('href', 'https://steam.granbluefantasy.com/');
    await expect(page.locator('.network-quality strong')).toHaveText(['36ms / 0%', '49ms / 10%']);
    await page.locator('.start-button').click(); await expect(page.locator('.start-button')).toHaveClass(/is-running/); await expect(page.getByLabel(t.mode, { exact: true })).toBeEnabled();
    await page.locator('.footer-controls').scrollIntoViewIfNeeded();
    for (const selector of ['.footer-controls', '.statusbar']) { const box = await page.locator(selector).boundingBox(); expect(box!.y).toBeGreaterThanOrEqual(0); expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height + 1); }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    if (viewport.height === 520) expect(await page.locator('.content').evaluate(e => e.scrollHeight <= e.clientHeight)).toBe(true);
  });
}

for (const language of ['zh-CN', 'zh-TW', 'ja', 'en'] as const) {
  test(`${language} acceleration controls fit the compact window`, async ({ page }) => {
    await openDesktop(page, {direct:true,configured:true,authorized:true});
    await page.setViewportSize({width:380,height:420});
    await menu(page);
    await page.getByRole('menuitemradio', {name:languageNames[language],exact:true}).click();
    await page.keyboard.press('Escape');
    const t = dictionaries[language];
    await page.getByLabel(t.mode,{exact:true}).selectOption('accelerate');
    await expect(page.getByLabel(t.line)).toHaveValue('auto:');
    await page.locator('.footer-controls').scrollIntoViewIfNeeded();
    for (const selector of ['.line-field','.footer-controls','.statusbar']) {
      const box = await page.locator(selector).boundingBox();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x+box!.width).toBeLessThanOrEqual(381);
      expect(box!.y+box!.height).toBeLessThanOrEqual(421);
    }
  });
}

for (const scale of [1, 1.25, 1.5, 2]) test.describe(`simulated Windows DPI ${scale * 100}%`, () => {
  test.use({ deviceScaleFactor: scale });
  test('constrained work area keeps the footer visible', async ({ page }) => {
    await openDesktop(page, { direct: true }); await page.setViewportSize({ width: 380, height: 420 });
    expect(await page.evaluate(() => devicePixelRatio)).toBe(scale);
    await page.locator('.start-button').click(); await page.locator('.footer-controls').scrollIntoViewIfNeeded();
    const box = await page.locator('.statusbar').boundingBox(); expect(box!.y).toBeGreaterThanOrEqual(0); expect(box!.y + box!.height).toBeLessThanOrEqual(421);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
});

async function cacheMenu(page: Page) {
  await menu(page); await page.locator('[data-action="cache-menu"]').click();
  return page.locator('#cache-menu');
}
async function auditDialog(page: Page) {
  await cacheMenu(page); await page.locator('[data-action="audit"]').click();
  const dialog = page.locator('.audit-dialog'); await expect(dialog).toBeVisible(); return dialog;
}

test('cache preferences persist independently while preserving an unsaved proxy URL', async ({ page }) => {
  await openDesktop(page); const draft = 'http://draft@localhost:8111';
  await page.getByLabel('代理 URL').fill(draft); await cacheMenu(page);
  await page.getByRole('menuitemcheckbox', { name: '素材預取' }).click();
  await expect(page.getByRole('menuitemcheckbox', { name: '素材預取' })).not.toBeChecked();
  await page.getByRole('menuitemcheckbox', { name: '記憶體預熱' }).click();
  await expect(page.getByRole('menuitemcheckbox', { name: '記憶體預熱' })).not.toBeChecked();
  await page.keyboard.press('Escape'); await page.keyboard.press('Escape'); await expect(page.getByLabel('代理 URL')).toHaveValue(draft);
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
  expect((await commands(page)).filter(c => c.command === 'save_cache_preferences').map(c => c.args.patch)).toEqual([{ prefetchEnabled: false }, { warmupEnabled: false }]);
  await page.reload(); await cacheMenu(page);
  await expect(page.getByRole('menuitemcheckbox', { name: '素材預取' })).not.toBeChecked();
  await expect(page.getByRole('menuitemcheckbox', { name: '記憶體預熱' })).not.toBeChecked();
});

test('cache preference save failure restores controls without overwriting URL drafts', async ({ page }) => {
  await openDesktop(page, { cacheDelay: 200, cacheFailure: true });
  await page.getByLabel('代理 URL').fill('socks5://draft@localhost:9911'); await cacheMenu(page);
  await page.getByRole('menuitemcheckbox', { name: '素材預取' }).click();
  await expect(page.getByRole('menuitemcheckbox', { name: '記憶體預熱' })).toBeDisabled();
  await expect(page.getByRole('menuitem', { name: '檢查快取…' })).toBeDisabled();
  await expect(page.getByRole('status')).toHaveText(dictionaries['zh-TW']['error.CONFIG_WRITE_FAILED']);
  await expect(page.getByRole('menuitemcheckbox', { name: '素材預取' })).toBeChecked();
  await expect(page.getByRole('menuitemcheckbox', { name: '素材預取' })).toBeEnabled();
  await page.keyboard.press('Escape'); await page.keyboard.press('Escape');
  await expect(page.getByLabel('代理 URL')).toHaveValue('socks5://draft@localhost:9911');
  expect((await commands(page)).filter(c => c.command === 'save_settings')).toHaveLength(0);
});

test('running allows background toggles but locks audit, clear and capacity', async ({ page }) => {
  await openDesktop(page, { direct: true }); await page.locator('.start-button').click(); await cacheMenu(page);
  await expect(page.getByRole('menuitem', { name: '檢查快取…' })).toBeDisabled();
  await expect(page.getByRole('menuitem', { name: '清理快取…' })).toBeDisabled(); await expect(page.getByLabel('上限')).toBeDisabled();
  await page.getByRole('menuitemcheckbox', { name: '記憶體預熱' }).click();
  await expect(page.getByRole('menuitemcheckbox', { name: '記憶體預熱' })).not.toBeChecked();
  await expect(page.locator('.start-button')).toHaveClass(/is-running/);
});

test('audit shows completed progress, closes and can be started again', async ({ page }) => {
  await openDesktop(page, { direct: true }); const dialog = await auditDialog(page);
  await expect(dialog.getByRole('status')).toContainText('檢查完成');
  await expect(dialog.getByRole('status')).toContainText('已檢查 8 · 已修復 2 · 失敗 0');
  await dialog.locator('[data-action="close-audit"]').click(); await expect(dialog).not.toBeVisible();
  await auditDialog(page); await expect(dialog.getByRole('status')).toContainText('檢查完成');
  expect((await commands(page)).filter(c => c.command === 'start_cache_audit')).toHaveLength(2);
});

test('running audit updates progress, blocks other actions and can cancel during maintenance', async ({ page }) => {
  await openDesktop(page, { direct: true, holdAudit: true }); const dialog = await auditDialog(page);
  await expect(dialog.getByRole('status')).toContainText('正在檢查快取');
  await expect(page.locator('.start-button')).toBeDisabled(); await expect(page.getByLabel('模式', { exact: true })).toBeDisabled();
  await expect(page.getByLabel('登入時啟動')).toBeDisabled();
  await page.keyboard.press('Escape'); await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('button', { name: '關閉對話框', exact: true })).toBeDisabled();
  await page.evaluate(() => { (window as any).__auditProgress(4, 1); (window as any).__emitNative({ state: { running: false, busy: false, maintenance: true, shuttingDown: false }, notice: null }); });
  await expect(dialog.getByRole('status')).toContainText('已檢查 4 · 已修復 1');
  await expect(dialog.locator('[data-action="cancel-audit"]')).toBeEnabled();
  await dialog.locator('[data-action="cancel-audit"]').click();
  await expect(dialog.getByRole('status')).toContainText('檢查已取消');
  await page.evaluate(() => (window as any).__emitNative({ state: { running: false, busy: false, maintenance: false, shuttingDown: false }, notice: null }));
  await dialog.locator('[data-action="close-audit"]').click(); await expect(page.locator('.start-button')).toBeEnabled();
});

test('audit startup failure is localized and leaves the dialog closable', async ({ page }) => {
  await openDesktop(page, { direct: true, startAuditFailure: true }); const dialog = await auditDialog(page);
  await expect(dialog.getByRole('status')).toContainText('檢查未完成');
  await expect(dialog.getByRole('alert')).toHaveText(dictionaries['zh-TW']['error.CACHE_AUDIT_FAILED']);
  await dialog.locator('[data-action="close-audit"]').click(); await expect(dialog).not.toBeVisible();
  await expect(page.locator('.start-button')).toBeEnabled();
});

test('audit cancellation failure retains active progress and cancellation control', async ({ page }) => {
  await openDesktop(page, { direct: true, holdAudit: true, cancelAuditFailure: true }); const dialog = await auditDialog(page);
  await dialog.locator('[data-action="cancel-audit"]').click();
  await expect(dialog.getByRole('alert')).toHaveText(dictionaries['zh-TW']['error.CACHE_AUDIT_FAILED']);
  await expect(dialog.getByRole('status')).toContainText('正在檢查快取');
  await expect(dialog.locator('[data-action="cancel-audit"]')).toBeEnabled(); await expect(page.locator('.start-button')).toBeDisabled();
});

test('audit failures remain visible alongside checked and repaired counts', async ({ page }) => {
  await openDesktop(page, { direct: true, auditFailures: 3 }); const dialog = await auditDialog(page);
  await expect(dialog.getByRole('status')).toContainText('檢查完成，部分項目處理失敗');
  await expect(dialog.getByRole('status')).toContainText('已檢查 8 · 已修復 2 · 失敗 3');
  await expect(dialog.locator('[data-action="close-audit"]')).toBeEnabled();
});

test('audit visibility pause preserves URL drafts and accepts completion after resume', async ({ page }) => {
  await openDesktop(page, { holdAudit: true }); await page.getByLabel('代理 URL').fill('http://draft@localhost:8855');
  const dialog = await auditDialog(page); await expect(dialog.getByRole('status')).toContainText('正在檢查快取');
  await page.evaluate(() => (window as any).__emitVisibility(false));
  const count = (await commands(page)).filter(c => c.command === 'get_status').length;
  await page.evaluate(() => (window as any).__finishAudit()); await page.waitForTimeout(1100);
  expect((await commands(page)).filter(c => c.command === 'get_status')).toHaveLength(count);
  await page.evaluate(() => (window as any).__emitVisibility(true));
  await expect(dialog.getByRole('status')).toContainText('檢查完成');
  await dialog.locator('[data-action="close-audit"]').click();
  await expect(page.getByLabel('代理 URL')).toHaveValue('http://draft@localhost:8855');
});

for (const language of ['zh-CN', 'zh-TW', 'ja', 'en'] as const) test(`${language} audit dialog fits a constrained work area`, async ({ page }) => {
  await openDesktop(page, { direct: true }); await page.setViewportSize({ width: 380, height: 300 }); await menu(page);
  await page.getByRole('menuitemradio', { name: languageNames[language], exact: true }).click(); await page.keyboard.press('Escape');
  const dialog = await auditDialog(page); const t = dictionaries[language];
  await expect(dialog.getByRole('status')).toContainText(t.auditDone);
  await expect(dialog.getByRole('status')).toContainText(translate(language, 'auditCounts', { checked: 8, repaired: 2, failed: 0 }));
  const box = await dialog.boundingBox(); expect(box!.x).toBeGreaterThanOrEqual(0); expect(box!.y).toBeGreaterThanOrEqual(0); expect(box!.x + box!.width).toBeLessThanOrEqual(380); expect(box!.y + box!.height).toBeLessThanOrEqual(300);
  const button = await dialog.locator('[data-action="close-audit"]').boundingBox(); expect(button!.y + button!.height).toBeLessThanOrEqual(300);
});
