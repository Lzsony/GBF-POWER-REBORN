// Injected only by the internal-test binary into its real WKWebView.
(() => {
  const run = async () => {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args);
    const control = (action, result) => invoke('internal_test_control', { action, result });
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const check = (value, message) => { if (!value) throw new Error(message); };
    const until = async (predicate, label, timeout = 10000) => {
      const end = Date.now() + timeout;
      while (Date.now() < end) { if (await predicate()) return; await sleep(50); }
      throw new Error(`Timeout: ${label}; status=${document.querySelector('.statusbar')?.textContent}`);
    };
    const checks = [];
    try {
      await control('report', { phase: 'script-started' });
      await until(() => document.querySelector('.start-button:not(:disabled)'), 'React ready');
      await document.fonts.ready;
      let initial = await invoke('get_status');
      const fixture = (await control('snapshot')).fixture;
        if (fixture?.stage === 'restart') {
          check(initial.preferences.language === 'zh-TW' && initial.preferences.theme === 'auto', 'preferences not persisted');
          check(initial.settings.listenPort === fixture.port, 'port not persisted');
          checks.push('schema1-restart-persistence');
        }
        document.querySelector('.menu-trigger').click();
        await until(() => document.querySelector('.language-options'), 'menu');
        if (fixture?.stage === 'fresh') {
          const input = document.querySelector('.menu-port input');
          Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, String(fixture.port));
          input.dispatchEvent(new Event('input', { bubbles: true }));
          await sleep(100);
          input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
          await until(async () => (await invoke('get_status')).settings.listenPort === fixture.port, 'port saved through React');
          initial = await invoke('get_status');
        }
        for (const language of ['zh-CN', 'zh-TW']) {
          const button = document.querySelector(`.language-options button[lang="${language}"]`);
          button.click();
          await until(() => button.getAttribute('aria-checked') === 'true' && button.getAttribute('aria-disabled') !== 'true', language);
          for (let theme = 0; theme < 3; theme++) {
            const item = document.querySelectorAll('.theme-options button')[theme];
            item.click();
            await until(() => item.getAttribute('aria-checked') === 'true' && item.getAttribute('aria-disabled') !== 'true', 'theme');
            check(document.documentElement.scrollWidth <= innerWidth, 'horizontal overflow');
            for (const node of document.querySelectorAll('.language-options button')) {
              const box = node.getBoundingClientRect();
              const range = document.createRange(); range.selectNodeContents(node);
              const text = range.getBoundingClientRect();
              check(text.left >= box.left && text.right <= box.right, 'language clipping');
            }
          }
        }
        document.querySelector('.menu-trigger').click();
        checks.push('two-languages-three-themes-native-layout');
        check(innerWidth===400 && innerHeight===520, 'native content size must be 400x520, got '+innerWidth+'x'+innerHeight+' '+JSON.stringify(await control('snapshot')));
        const change = (node,value) => {
          const proto=node instanceof HTMLSelectElement?HTMLSelectElement.prototype:HTMLInputElement.prototype;
          Object.getOwnPropertyDescriptor(proto,'value').set.call(node,value);
          node.dispatchEvent(new Event(node instanceof HTMLSelectElement?'change':'input',{bubbles:true}));
        };
        const layout = () => {
          const content=document.querySelector('.content'), footer=document.querySelector('.statusbar').getBoundingClientRect();
          check(content.scrollHeight<=content.clientHeight, 'content vertical overflow');
          check(document.documentElement.scrollWidth<=innerWidth, 'horizontal overflow');
          check(footer.top>=0 && footer.bottom<=innerHeight, 'footer clipped');
        };
        change(document.querySelector('.mode-field select'),'proxy');
        await until(()=>document.querySelector('.host-field input:not(:disabled)'), 'proxy row');
        const proxyInput=document.querySelector('.host-field input');
        const oldURL=(await invoke('get_status')).settings.proxyUrl;
        const draft='http://127.0.0.1:'+fixture.proxyTestPort;
        change(proxyInput,draft);await sleep(100);proxyInput.dispatchEvent(new FocusEvent('blur',{bubbles:true}));
        check((await invoke('get_status')).settings.proxyUrl===oldURL,'blur saved draft');
        document.querySelector('.connection-test').click();
        await until(()=>document.querySelector('.connection-test').textContent==='保存','port test succeeded');
        check(document.querySelector('.statusbar').textContent.includes('TCP 連線成功，請保存'),'port status missing');
        check((await invoke('get_status')).settings.proxyUrl===oldURL,'test saved draft');layout();
        document.querySelector('.connection-test').click();
        await until(async()=> (await invoke('get_status')).settings.proxyUrl===draft, 'explicit save');
        await until(()=>document.querySelector('.connection-test').textContent==='測試'&&!document.querySelector('.mode-field select').disabled,'save done');
        check(document.querySelector('.mode-field select option[value="accelerate"]').disabled, 'acceleration option must be disabled');
        change(document.querySelector('.mode-field select'),'direct');
        await until(()=>!document.querySelector('.line-field')&&!document.querySelector('.host-field'),'restore direct');layout();
        checks.push('400x520-available-modes-no-scroll', 'acceleration-disabled', 'port-only-test-then-explicit-save', 'footer-version-visible');

        await control('autostart-on');
        check((await control('snapshot')).autostart, 'autostart registration missing');
        await control('autostart-off');
        check(!(await control('snapshot')).autostart, 'autostart removal failed');
        checks.push('isolated-autostart-registration-removal');
        await control('hide'); await sleep(1400);
        const before = (await control('snapshot')).statusReads;
        await sleep(1600);
        check((await control('snapshot')).statusReads === before, 'hidden webview still polling');
        await control('tray-start');
        await until(async () => (await invoke('get_native_control')).state.running, 'hidden start');
        check(!(await control('snapshot')).visible, 'hidden start opened window');
        await control('report', { phase: 'pac', port: initial.settings.listenPort });
        await until(async () => (await control('snapshot')).fixture?.pacPassed, 'external PAC probe');
        await control('tray-stop');
        await until(async () => !(await invoke('get_native_control')).state.running, 'hidden stop');
        await control('show');
        await until(async () => (await control('snapshot')).statusReads > before, 'poll resumes');
        checks.push('hidden-polling-suspended', 'hidden-tray-start-stop', 'pac-http', 'visible-polling-resumes');
      await control('report', { passed: true, checks, userAgent: navigator.userAgent });
      await sleep(300);
      document.querySelector('.menu-trigger').click();
      await until(()=>document.querySelector('[data-action="quit"]'),'quit menu');
      document.querySelector('[data-action="quit"]').click();
      check(!document.querySelector('dialog[open]'),'quit confirmation appeared');
    } catch (error) {
      await control('report', { passed: false, checks, error: String(error) });
      await invoke('stop_proxy').catch(() => {});
      await control('autostart-off').catch(() => {});
      await invoke('quit_app').catch(() => {});
    }
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', run, { once: true }); else void run();
})();
