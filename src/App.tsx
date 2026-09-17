import { version } from '../package.json' with { type: 'json' };
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { visibilityPoll } from './polling';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import SettingsMenu, { type MenuAction } from './SettingsMenu';
import { useLineTests } from './useLineTests';
import { useProxyUrl } from './useProxyUrl';
import appLogo from '../src-tauri/icons/icon.png';
import { initialSettings, initialStatus, type Settings, type Status, type Preferences, type CachePreferences } from './types';
import { errorMessage, languageFromSystem, translate, type Message, type MessageKey } from './i18n';

const native = isTauri();
function duration(n: number) { return [Math.floor(n / 3600), Math.floor(n / 60) % 60, n % 60].map(v => String(v).padStart(2, '0')).join(':'); }

export default function App() {
  const [status, setStatus] = useState<Status>(initialStatus);
  const [settings, setSettings] = useState<Settings>(initialSettings);
  const [preferences, setPreferences] = useState<Preferences>({ theme: 'auto', language: languageFromSystem(navigator.language) });
  const [preferencesBusy, setPreferencesBusy] = useState(false);
  const [systemDark, setSystemDark] = useState(matchMedia('(prefers-color-scheme: dark)').matches);
  const [ready, setReady] = useState(!native);
  const [busy, setBusy] = useState<MessageKey | null>(null);
  const [coreBusy, setCoreBusy] = useState(false);
  const nativeNotice = useRef(0);
  const [notice, setNotice] = useState<Message>({ key: native ? 'loading' : 'preview' });
  const [error, setError] = useState<MessageKey | null>(null);
  const [flash, setFlash] = useState<Message | null>(null);
  const [cacheBusy, setCacheBusy] = useState(false);
  const cacheSaving = useRef(false);
  const [auditOpen, setAuditOpen] = useState(false);
  const [auditPending, setAuditPending] = useState<'start' | 'cancel' | null>(null);
  const [auditError, setAuditError] = useState<MessageKey | null>(null);
  const auditAction = useRef(false);
  const auditDialog = useRef<HTMLDialogElement>(null);
  const [authorizationOpen, setAuthorizationOpen] = useState(false);
  const authorizationDialog = useRef<HTMLDialogElement>(null);
  const [accessCode, setAccessCode] = useState('');
  const [authRegistered, setAuthRegistered] = useState(false);
  const [authLoading, setAuthLoading] = useState(false);
  const [authLoaded, setAuthLoaded] = useState(false);
  const authRevision = useRef(0);
  const appliedSettings = useRef(initialSettings);
  const taskRunning = useRef(false);
  const [rates, setRates] = useState<{down:number|null;up:number|null}>({ down: null, up: null });
  const previous = useRef<{ received: number; sent: number; time: number } | null>(null);
  const initialized = useRef(false);
  const t = (key: MessageKey, args?: Record<string, string | number>) => translate(preferences.language, key, args);
  const number = (n: number, decimals = 0) => new Intl.NumberFormat(preferences.language, { minimumFractionDigits: decimals, maximumFractionDigits: decimals }).format(n);
  const size = (n: number) => n >= 1024 ** 3 ? `${number(n / 1024 ** 3, 2)} GB` : n >= 1024 ** 2 ? `${number(n / 1024 ** 2, 1)} MB` : `${number(n / 1024, 1)} KB`;
  const speed = (n: number|null) => n === null ? '—' : n >= 1024 ** 2 ? `${number(n / 1024 ** 2, 1)} MB/s` : `${number(n / 1024, 1)} KB/s`;
  const proxyUrl = useProxyUrl(settings, ready, setError);
  const lineTests = useLineTests(status);
  const saving = useRef(false);
  const taskLabel=useRef<MessageKey|null>(null);
  const actualDark = preferences.theme === 'dark' || (preferences.theme === 'auto' && systemDark);
  const certificateLabel: MessageKey = status.certificate.trusted ? 'trusted' : status.certificate.exists ? 'untrusted' : 'missing';

  const [proxyTest,setProxyTest]=useState<'idle'|'testing'|'ready'|'saving'>('idle');
  const [testedUrl,setTestedUrl]=useState<string|null>(null);
  const [testNotice,setTestNotice]=useState<Message|null>(null);
  const proxyTestId=useRef<string|null>(null);
  const quitting=useRef(false);
  const [isQuitting,setIsQuitting]=useState(false);
  const locked = status.audit.running || auditPending !== null || cacheBusy || coreBusy || !!busy || !ready || isQuitting;
  const backendDisabled = !native || locked;
  const proxyActionBusy=proxyTest==='testing'||proxyTest==='saving';
  const testPending=proxyActionBusy||lineTests.pending!==null;
  function clearProxyTest(){const id=proxyTestId.current;proxyTestId.current=null;if(id)void invoke('cancel_proxy_test',{testId:id}).catch(()=>{});setProxyTest('idle');setTestedUrl(null);setTestNotice(null);}
  useEffect(()=>{clearProxyTest();},[settings.mode,status.running]);
  useEffect(()=>{if(proxyTest==='ready')clearProxyTest();},[status.settings.proxyUrl,status.settings.hasAuthentication]);
  useEffect(()=>()=>{const id=proxyTestId.current;if(id)void invoke('cancel_proxy_test',{testId:id}).catch(()=>{});},[]);
  const lineName = (id: string) => status.accelerationLines.find(line=>line.id===id)?.name ?? id;
  const selectedLineName = status.acceleration.lineId ? lineName(status.acceleration.lineId) : null;
  const connectionStatus=!ready?t('loading'):!status.running?(status.acceleration.error?t(errorMessage({code:status.acceleration.error})):t('stoppedNotice')):status.settings.mode==='accelerate'?(status.authorization.state==='unavailable'?t('authorization.unavailable'):status.acceleration.error?t(errorMessage({code:status.acceleration.error})):`${t(`acceleration.${status.acceleration.state}`)}${selectedLineName?` · ${selectedLineName}`:''}`):t(status.settings.mode==='direct'?'runningDirect':'runningProxy');
  const manualStatus = lineTests.result ? lineTests.result.state==='failed'?t('lineTestFailed'):lineTests.result.medianMs===null?'':`${lineTests.result.automatic?`${lineName(lineTests.result.lineId)} · `:''}${number(lineTests.result.medianMs,Number.isInteger(lineTests.result.medianMs)?0:1)} ms` : '';
  useEffect(()=>{if(!['loaded','loading','preview'].includes(notice.key))setFlash(notice);},[notice]);
  const progressStatus=busy&&['connection','savingSettings'].includes(busy)&&['selecting','connecting'].includes(status.acceleration.state)?`${t(`acceleration.${status.acceleration.state}`)}${selectedLineName?` · ${selectedLineName}`:''}`:busy?t('busy',{action:t(busy)}):'';
  useLayoutEffect(() => {
    document.documentElement.dataset.theme = actualDark ? 'dark' : 'light';
    document.documentElement.style.colorScheme = actualDark ? 'dark' : 'light';
    document.documentElement.lang = preferences.language;
  }, [actualDark, preferences.language]);
  useEffect(() => {
    const media = matchMedia('(prefers-color-scheme: dark)');
    const changed = () => setSystemDark(media.matches);
    media.addEventListener('change', changed);
    return () => media.removeEventListener('change', changed);
  }, []);


  const acceptStatus = useCallback((next: Status) => {
    const now = performance.now();
    const old = previous.current;
    if (old && next.running && next.metrics.received >= old.received && next.metrics.sent >= old.sent) {
      const seconds = Math.max((now - old.time) / 1000, 0.001);
      setRates({ down: (next.metrics.received - old.received) / seconds, up: (next.metrics.sent - old.sent) / seconds });
    } else setRates({ down: null, up: null });
    previous.current = next.running ? { received: next.metrics.received, sent: next.metrics.sent, time: now } : null;
    appliedSettings.current = next.settings;
    setStatus(next);
    if (!initialized.current) {
      setSettings(next.settings); setPreferences(next.preferences); initialized.current = true; setReady(true); setNotice({ key: 'loaded' });
    }
    return next;
  }, []);
  const refresh = useCallback(async () => {
    if (!native) return;
    return acceptStatus(await invoke<Status>('get_status'));
  }, [acceptStatus]);
  useEffect(() => {
    if (!native) return;
    type Snapshot = { state: {running:boolean;busy:boolean;maintenance:boolean;shuttingDown:boolean}; notice: {id:number;code:string}|null };
    let live = true;
    let revision = 0;
    let unlisten: (() => void) | undefined;
    const apply = (next: Snapshot) => {
      if (!live || !next?.state) return;
      setCoreBusy(next.state.busy || next.state.maintenance || next.state.shuttingDown);
      setStatus(old => ({...old, running:next.state.running}));
      if (next.notice && next.notice.id > nativeNotice.current) {
        nativeNotice.current = next.notice.id;
        setError(errorMessage(next.notice));
        void invoke('acknowledge_native_notice', {id:next.notice.id}).catch(() => {});
      }
    };
    void (async () => {
      try {
        unlisten = await listen<Snapshot>('native-control', event => {revision++; apply(event.payload);});
        if (!live) {unlisten();return;}
        const stamp = revision;
        const next = await invoke<Snapshot>('get_native_control');
        if (stamp === revision) apply(next);
      } catch (e) {if(live)setError(errorMessage(e));}
    })();
    return () => {live=false;unlisten?.();};
  }, []);
  useEffect(() => {
    if (!native) return;
    let live = true;
    let unlisten: (() => void) | undefined;
    let revision = 0;
    const poll = visibilityPoll(() => invoke<Status>('get_status'), acceptStatus,
      e => setError(errorMessage(e)), () => { previous.current = null; setRates({ down: null, up: null }); });
    void (async () => {
      try {
        unlisten = await listen<boolean>('main-visibility', event => { revision++; poll.visibility(event.payload); });
        if (!live) { unlisten(); return; }
        const stamp = revision;
        const visible = await invoke<boolean>('main_window_visible');
        if (live && stamp === revision) poll.visibility(visible);
      } catch (e) { if (live) setError(errorMessage(e)); }
    })();
    return () => { live = false; unlisten?.(); poll.dispose(); };
  }, [acceptStatus]);
  useEffect(() => { if (!flash) return; const timer = setTimeout(() => setFlash(null), 2200); return () => clearTimeout(timer); }, [flash]);

  useEffect(() => {
    const dialog = auditDialog.current;
    if (auditOpen) dialog?.showModal(); else dialog?.close();
  }, [auditOpen]);
  useEffect(() => {
    const dialog = authorizationDialog.current;
    if (authorizationOpen) dialog?.showModal(); else dialog?.close();
  }, [authorizationOpen]);

  async function changeCachePreferences(patch: Partial<CachePreferences>) {
    if (!native || cacheSaving.current || locked || proxyActionBusy) return;
    cacheSaving.current = true; setCacheBusy(true); setError(null);
    try {
      const cachePreferences = await invoke<CachePreferences>('save_cache_preferences', { patch });
      setStatus(previous => ({ ...previous, cachePreferences }));
    } catch (e) {
      setError(errorMessage(e));
      await refresh().catch(() => undefined);
    } finally { cacheSaving.current = false; setCacheBusy(false); }
  }
  function closeAudit() {
    if (!status.audit.running && !auditPending) setAuditOpen(false);
  }
  async function auditOperation(action: 'start' | 'cancel') {
    if (auditAction.current || quitting.current || !native) return;
    if (action === 'start' && (locked || saving.current || taskRunning.current || status.running || proxyActionBusy)) return;
    auditAction.current = true; setAuditPending(action); setAuditError(null); setError(null);
    if (action === 'start') {
      setAuditOpen(true);
      setStatus(previous => ({ ...previous, audit: { running: true, cancelled: false, checked: 0, repaired: 0, failed: 0 } }));
    }
    let accepted = false;
    try {
      await invoke(action === 'start' ? 'start_cache_audit' : 'cancel_cache_audit');
      accepted = true;
      await refresh();
    } catch (e) {
      const key = errorMessage(e); setAuditError(key); setError(key);
      if (action === 'start' && !accepted) setStatus(previous => ({ ...previous, audit: { ...previous.audit, running: false } }));
      await refresh().catch(() => undefined);
    } finally { auditAction.current = false; setAuditPending(null); }
  }

  async function loadAuthorization(revision: number) {
    setAuthLoading(true); setAuthLoaded(false);
    try {
      const value = await invoke<{registered:boolean;code:string|null}>('get_authorization_dialog');
      if (revision !== authRevision.current) return;
      setAuthRegistered(value.registered); setAccessCode(value.code ?? ''); setAuthLoaded(true);
    } catch (e) { if (revision === authRevision.current) setError(errorMessage(e)); }
    finally { if (revision === authRevision.current) setAuthLoading(false); }
  }
  function openAuthorization() {
    if (!native || !status.authorization.configured) return;
    const revision = ++authRevision.current;
    setAccessCode(''); setAuthRegistered(false); setAuthLoaded(false); setError(null);
    setAuthorizationOpen(true);
    void loadAuthorization(revision);
    void invoke('refresh_authorization').then(refresh).catch(e => {
      if (revision === authRevision.current) setError(errorMessage(e));
    });
  }
  function closeAuthorization() {
    if (authLoading || busy) return;
    authRevision.current++;
    setAccessCode(''); setAuthLoaded(false); setAuthorizationOpen(false);
  }
  async function submitAuthorization() {
    if (!authLoaded || authLoading || busy || status.running) return;
    await task('authorization', async () => {
      if (authRegistered) {
        await invoke('unbind_authorization');
        setAuthRegistered(false); setAccessCode('');
      } else {
        await invoke('activate_authorization', {code:accessCode.trim()});
        await loadAuthorization(authRevision.current);
      }
    });
  }

  async function saveNow(patch:Partial<Settings>, url:string|null=null):Promise<boolean> {
    if(saving.current || quitting.current)return false;
    if(!native){setSettings(previous=>({...previous,...patch}));return true;}
    const base=appliedSettings.current;const next={...base,...patch};
    if (next.mode==='accelerate' && (!status.authorization.configured || status.authorization.state!=='active')) {
      if (status.authorization.configured) openAuthorization();
      return false;
    }
    if(url===null && Object.entries(patch).every(([key,value])=>base[key as keyof Settings]===value))return true;
    saving.current=true;setBusy('savingSettings');setError(null);
    try {
      const {proxyUrl:_url,hasAuthentication:_auth,...input}=next;
      await invoke('save_settings',{input:{...input,proxyUrl:url}});
      const saved=await refresh();if(saved){setSettings(s=>({...s,...Object.fromEntries(Object.keys(patch).map(key=>[key,saved.settings[key as keyof Settings]])),proxyUrl:saved.settings.proxyUrl,hasAuthentication:saved.settings.hasAuthentication}));if(url!==null)proxyUrl.saved(saved.settings);else if(patch.mode && patch.mode!=='proxy')proxyUrl.reset();}
      return true;
    }catch(e){const key=errorMessage(e);setError(key);const saved=await refresh().catch(()=>undefined);if(saved){setSettings(s=>({...s,...Object.fromEntries(Object.keys(patch).map(k=>[k,saved.settings[k as keyof Settings]]))}));if(key==='error.SWITCH_FAILED_RESTORED'||key==='error.SWITCH_RESTORE_FAILED')proxyUrl.reset();}return false;
    }finally{saving.current=false;setBusy(taskLabel.current);}
  }
  function field<K extends keyof Settings>(key:K,value:Settings[K]) {
    void saveNow({[key]:value}).catch(e=>setError(errorMessage(e)));
  }
  async function commitNumber(key:'listenPort'|'cacheLimitGb') {
    const value=settings[key];const valid=Number.isInteger(value)&&(key==='listenPort'?value>=1024&&value<=65535:value>=1&&value<=100);
    if(!valid){setError(key==='listenPort'?'error.LISTEN_PORT_INVALID':'error.CACHE_LIMIT_INVALID');return false;}
    return saveNow({[key]:value});
  }
  async function proxyAction() {
    if(!native||proxyActionBusy||locked||quitting.current)return;
    setError(null);setFlash(null);
    if(proxyTest==='ready'&&testedUrl===proxyUrl.value){
      setProxyTest('saving');
      const saved=await saveNow({mode:'proxy'},proxyUrl.value);
      if(saved){setTestedUrl(null);setProxyTest('idle');setTestNotice({key:'proxySaved'});}
      else setProxyTest('ready');
      return;
    }
    const id=crypto.randomUUID();const draft=proxyUrl.value;
    proxyTestId.current=id;setProxyTest('testing');setTestNotice({key:'testingConnection'});
    try{
      const {proxyUrl:_url,hasAuthentication:_auth,...input}=appliedSettings.current;
      const result=await invoke<{testId:string;connected:boolean;state:string}>('test_proxy_url',{testId:id,input:{...input,mode:'proxy',proxyUrl:draft}});
      if(proxyTestId.current!==id||result.testId!==id)return;
      if(result.state==='cancelled'){setProxyTest('idle');setTestNotice(null);return;}
      setTestedUrl(result.connected?draft:null);setProxyTest(result.connected?'ready':'idle');setTestNotice({key:result.connected?'proxyPortSuccess':'proxyPortFailure'});
    }catch{if(proxyTestId.current===id){setProxyTest('idle');setTestedUrl(null);setTestNotice({key:'proxyPortFailure'});}}
    finally{if(proxyTestId.current===id)proxyTestId.current=null;}
  }
  function testSelectedLine() {
    if (testPending || locked || !native || !status.authorization.configured || status.accelerationLines.length===0) return;
    const automatic = settings.lineSelection==='auto';
    const lineId = automatic ? (status.running ? status.acceleration.lineId : null) : settings.selectedLineId;
    if (automatic && status.running && !lineId) return;
    setError(null); setFlash(null); setTestNotice(null);
    void lineTests.test(lineId, automatic);
  }
  function exitNow(){
    if(quitting.current||!native)return;
    quitting.current=true;setIsQuitting(true);clearProxyTest();lineTests.clear();
    void invoke('quit_app').catch(e=>{quitting.current=false;setIsQuitting(false);setError(errorMessage(e));});
  }
  async function task(label: MessageKey, work: () => Promise<unknown>) {
    if(taskRunning.current)return; taskRunning.current=true;
    taskLabel.current=label;setBusy(label); setError(null);
    try { await work(); } catch (e) { setError(errorMessage(e)); }
    finally { try { await refresh(); } catch (e) { setError(errorMessage(e)); } taskRunning.current=false;taskLabel.current=null;setBusy(saving.current?'savingSettings':null); }
  }
  async function toggle() {
    if(status.running){await invoke('stop_proxy');setNotice({key:'stoppedNotice'});return;}
    if(!await commitNumber('listenPort')||!await commitNumber('cacheLimitGb'))return;
    if(settings.mode==='proxy'&&proxyUrl.edited)return;
    if(settings.mode==='accelerate'&&status.authorization.state!=='active'){openAuthorization();return;}
    await invoke('start_proxy');setNotice({key:'startedNotice'});
  }
  async function changePreferences(next: Preferences) {
    const before = preferences;
    setPreferences(next); setError(null);
    if (!native) return;
    setPreferencesBusy(true);
    try { await invoke('save_preferences', { preferences: next }); setNotice({ key: 'preferencesSaved' }); }
    catch (e) { setError(errorMessage(e)); try { setPreferences((await refresh())?.preferences ?? before); } catch { setPreferences(before); } }
    finally { setPreferencesBusy(false); }
  }
  function menuAction(action: MenuAction) {
    if (action === 'authorization') { openAuthorization(); return; }
    if (action === 'audit') { void auditOperation('start'); return; }
    if (action === 'clear') { void task('clearCache', async()=>{await invoke('clear_cache');setNotice({key:'cacheCleared'});});return; }
    if (action === 'quit') { exitNow(); return; }
    if (action === 'data' || action === 'openCertificate') {
      void task(action === 'data' ? 'openDirectory' : 'openCertificate', () => invoke('open_local', { kind: action === 'data' ? 'data' : 'certificate' })); return;
    }
    void task('certificate', async () => {
      await invoke('manage_certificate', { action });
      if (action === 'remove') field('httpsCache', false);
      setNotice({ key: action === 'remove' ? 'certificateRemoved' : 'certificateUpdated' });
    });
  }


  return <main className="app-shell" data-running={status.running}>
    <div className="content">
      <section className="running-row" aria-label="GBF POWER REBORN">
        <div className="brand"><img src={appLogo} alt=""/><strong>GBF POWER REBORN</strong></div>
        <SettingsMenu authorizationConfigured={status.authorization.configured} authorizationState={t(`authorization.${status.authorization.state}`)} cachePreferences={status.cachePreferences} cacheBusy={cacheBusy} auditRunning={status.audit.running} onCachePreference={changeCachePreferences} quitting={isQuitting} cacheUsage={size(status.cacheBytes)} onCacheLimit={value => setSettings(s=>({...s,cacheLimitGb:value}))} onCommitCacheLimit={()=>void commitNumber('cacheLimitGb')} t={t} preferences={preferences} certificate={status.certificate} settings={settings} running={status.running} busy={locked || proxyActionBusy} preferencesBusy={preferencesBusy} ready={ready} native={native} onPreference={changePreferences} onListenPort={value => setSettings(s=>({...s,listenPort:value}))} onCommitListenPort={()=>void commitNumber('listenPort')} onCopyPac={() => void task('copyPac', async () => { await writeText(status.pacUrl); setNotice({ key: 'copied' }); setFlash({ key: 'copied' }); })} onAction={menuAction}/>
      </section>

      <section className="connection-section">
        <div className="form-row connection-main">
          <label className="inline-field mode-field"><span>{t('mode')}</span><select aria-label={t('mode')} value={settings.mode} disabled={locked||testPending||isQuitting} onChange={e => field('mode', e.target.value as Settings['mode'])}><option value="direct">{t('direct')}</option><option value="proxy">{t('proxy')}</option><option value="accelerate" disabled={!status.authorization.configured}>{status.authorization.configured?t('accelerate'):t('accelerateUnavailable')}</option></select></label>
          {settings.mode === 'proxy' && <div className="inline-field host-field"><span>URL</span><input aria-label={t('proxyUrl')} type="text" value={proxyUrl.value} disabled={locked||proxyActionBusy||isQuitting} onChange={e=>{clearProxyTest();proxyUrl.change(e.target.value);}} onKeyDown={e=>{if(e.key==='Enter'){e.preventDefault();void proxyAction();}}} spellCheck={false} autoComplete="off" placeholder={proxyUrl.loading?t('loadingProxy'):'socks5://127.0.0.1:7890'}/><button className="connection-test" disabled={backendDisabled||testPending||proxyUrl.unavailable||isQuitting} aria-busy={proxyActionBusy} onClick={()=>void proxyAction()}>{t(proxyTest==='ready'||proxyTest==='saving'?'saveProxy':'lineTest')}</button></div>}
          {settings.mode==='accelerate'&&status.authorization.configured&&<div className="inline-field line-field"><span>{t('line')}</span><select aria-label={t('line')} value={settings.lineSelection==='auto'?'auto:':settings.selectedLineId} disabled={locked||testPending||isQuitting} onChange={e=>void saveNow(e.target.value==='auto:'?{lineSelection:'auto'}:{lineSelection:'manual',selectedLineId:e.target.value})}><option value="auto:">{status.running&&selectedLineName?t('automaticLineNamed',{name:selectedLineName}):t('automaticLine')}</option>{settings.lineSelection==='manual'&&!status.accelerationLines.some(line=>line.id===settings.selectedLineId)&&<option value={settings.selectedLineId} disabled>{t('lineUnavailable')}</option>}{status.accelerationLines.map(line=><option key={line.id} value={line.id}>{line.name}</option>)}</select><button className="connection-test" aria-label={t('lineTest')} aria-busy={lineTests.pending!==null} disabled={backendDisabled||testPending||status.accelerationLines.length===0||(settings.lineSelection==='auto'&&status.running&&!status.acceleration.lineId)} onClick={testSelectedLine}>{t('lineTest')}</button></div>}

        </div>

      </section>

      <section className="cache-section">
        <div className="form-row cache-first">
          <label className="checkbox"><input type="checkbox" checked={settings.httpsCache} disabled={locked || (!status.certificate.trusted && !settings.httpsCache)} onChange={e => field('httpsCache', e.target.checked)}/><span>{t('httpsCache')}</span></label>
          <span className="certificate-state">{t('certificate')} · <span className={status.certificate.trusted ? 'accent' : 'muted'}>{t(certificateLabel)}</span></span>
        </div>

      </section>

      <section className="statistics-section">
        <div className="statistics">
          <Metric label={t('routeLatency')} value={status.running&&status.settings.mode==='accelerate'&&status.acceleration.state==='connected'&&status.metrics.network.lineLatencyMs!==null?`${number(status.metrics.network.lineLatencyMs)} ms`:'—'}/>
          <Metric label={t('requests')} value={number(status.metrics.requests)}/>
          <div className="cache-summary"><Metric label={t('downloads')} value={number(status.metrics.downloads)}/><Metric label={t('hitRate')} value={status.metrics.hitRate===null?'—':`${number(status.metrics.hitRate,1)}%`}/></div>
          <div className="metric traffic"><span>{t('traffic')}</span><strong className="numeric"><span>↓ {speed(rates.down)}</span><span>↑ {speed(rates.up)}</span></strong></div>


        </div>
      </section>

      <section className="latency-section" aria-label={t('gameVersion')}>
        <div className="latency-heading"><span>{t('gameVersion')}</span><span>{t('latencyLabel')}/{t('timeoutLabel')}</span></div>
          <div className="network-row">{(['mobage','steam'] as const).map(site=>{const n=status.metrics.network;return <div className="metric network-quality" key={site}><a href={site==='mobage'?'https://game.granbluefantasy.jp/':'https://steam.granbluefantasy.com/'} onClick={e=>{if(!native)return;e.preventDefault();void invoke('open_game_site',{site}).catch(e=>setError(errorMessage(e)));}}>{t(site==='mobage'?'gameDirectLatency':'steamDirectLatency')}</a><strong className="numeric">{site==='mobage'?formatQuality(n.gameMedianMs,n.gameTimeoutPercent,number):formatQuality(n.steamMedianMs,n.steamTimeoutPercent,number)}</strong></div>})}</div>
      </section>
      <section className="start-row">
        <button className={`primary start-button${status.running ? ' is-running' : ''}`} disabled={backendDisabled || (!status.running && (proxyUrl.unavailable||testPending||(settings.mode==='proxy'&&proxyUrl.edited)))||isQuitting} onClick={() => void task('connection', toggle)}>
          {status.running ? <><StopIcon/><span className="numeric">{duration(status.metrics.uptimeSecs)}</span></> : <><StartIcon/><span>{t('start')}</span></>}
        </button>
      </section>

      <section className="footer-controls">
        <label className="checkbox"><input type="checkbox" checked={settings.autostart} disabled={locked} onChange={e => field('autostart', e.target.checked)}/><span>{t('autostart')}</span></label>
      </section>
    </div>
      <footer className="statusbar"><span role="status" className={error ? 'error' : ''}>{error ? t(error) : isQuitting ? t('quitting') : busy ? progressStatus : lineTests.pending!==null?t('testingConnection') : testNotice ? t(testNotice.key,testNotice.args) : manualStatus || (flash ? t(flash.key,flash.args) : connectionStatus)}</span><span>v{version}</span></footer>
    <dialog ref={auditDialog} className="audit-dialog" aria-label={t('auditTitle')} onCancel={event => { event.preventDefault(); closeAudit(); }}>
      <div className="dialog-heading"><h2>{t('auditTitle')}</h2><button type="button" className="text-button" aria-label={t('closeDialog')} disabled={status.audit.running || auditPending !== null} onClick={closeAudit}><svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" stroke="currentColor" strokeWidth="2"/></svg></button></div>
      <p role="status">{t(auditPending === 'start' ? 'auditStarting' : auditPending === 'cancel' ? 'auditCancelling' : status.audit.running ? 'auditRunning' : status.audit.cancelled ? 'auditCancelled' : auditError ? 'auditFailed' : status.audit.failed > 0 ? 'auditIssues' : 'auditDone')}<br/>{t('auditCounts', { checked: number(status.audit.checked), repaired: number(status.audit.repaired), failed: number(status.audit.failed) })}</p>
      {auditError && <p role="alert" className="error">{t(auditError)}</p>}
      <div className="dialog-actions"><button type="button" data-action={status.audit.running || auditPending !== null ? "cancel-audit" : "close-audit"} disabled={auditPending !== null || isQuitting} onClick={() => { if (status.audit.running) void auditOperation('cancel'); else closeAudit(); }}>{t(status.audit.running || auditPending !== null ? 'cancel' : 'closeDialog')}</button></div>
    </dialog>
    <dialog ref={authorizationDialog} className="authorization-dialog" aria-label={t('authorization')} onCancel={event => {event.preventDefault();closeAuthorization();}}>
      <div className="dialog-heading"><h2>{t('authorization')}</h2><button type="button" className="text-button" aria-label={t('closeDialog')} disabled={authLoading||!!busy} onClick={closeAuthorization}><svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" stroke="currentColor" strokeWidth="2"/></svg></button></div>
      <div className="auth-fields">
        <div className="authorization-validity"><span>{t('authorization')}</span><strong>{t(`authorization.${status.authorization.state}`)}</strong></div>
        <label>{t('accessCode')}<input aria-label={t('accessCode')} type="password" autoComplete="off" spellCheck={false} value={accessCode} readOnly={authRegistered} disabled={!authLoaded||authLoading||(!authRegistered&&(status.running||!!busy))} onChange={event=>setAccessCode(event.target.value)}/></label>
        <p role={error?'alert':'status'} className={error?'error':''}>{error?t(error):authLoading?t('loading'):status.running?t('stopToEdit'):t(`authorization.${status.authorization.state}`)}</p>
        <div className="dialog-actions"><button type="button" className="primary" disabled={!authLoaded||authLoading||status.running||!!busy||(!authRegistered&&!accessCode.trim())} onClick={()=>void submitAuthorization()}>{t(authRegistered?'unbindDevice':'activateAuthorization')}</button></div>
      </div>
    </dialog>
  </main>;
}
function Metric({ label, value, className = '' }: { label: string; value: string; className?: string }) {
  return <div className={`metric ${className}`}><span>{label}</span><div><strong className="numeric">{value}</strong></div></div>;
}

function formatQuality(median:number|null,timeout:number|null,format:(n:number,decimals?:number)=>string) {
  const ms=median===null?'—':format(median,Number.isInteger(median)?0:1)+'ms';
  const percent=timeout===null?'—':format(Math.round(timeout*10)/10,Number.isInteger(Math.round(timeout*10)/10)?0:1)+'%';
  return `${ms} / ${percent}`;
}
function StartIcon(){return <svg viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M7 4v16l13-8z"/></svg>;}
function StopIcon(){return <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="1" fill="currentColor"/></svg>;}
