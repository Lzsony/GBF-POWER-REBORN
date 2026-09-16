import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react';
import type { CachePreferences, CertificateStatus, Preferences, Settings } from './types';
import type { MessageKey } from './i18n';

export type MenuAction = 'install' | 'openCertificate' | 'check' | 'remove' | 'clear' | 'data' | 'quit' | 'audit';
type Props = {
  quitting:boolean;
  onCommitListenPort:()=>void;onCommitCacheLimit:()=>void;
  cachePreferences: CachePreferences; cacheBusy: boolean; auditRunning: boolean;
  onCachePreference: (patch: Partial<CachePreferences>) => Promise<void>;
  cacheUsage: string; onCacheLimit: (value: number) => void;
  t: (key: MessageKey) => string; preferences: Preferences; certificate: CertificateStatus;
  settings: Settings;
  running: boolean; busy: boolean; preferencesBusy: boolean; ready: boolean; native: boolean;
  onPreference: (next: Preferences) => Promise<void>; onListenPort: (value: number) => void;
  onCopyPac: () => void; onAction: (action: MenuAction) => void;
};
export function ThemeIcon({ theme }: { theme: Preferences['theme'] }) {
  return <svg viewBox="0 0 24 24" width="19" height="19" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
    {theme === 'light' ? <><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5"/></> : theme === 'dark' ? <path d="M20 15.2A8.6 8.6 0 0 1 8.8 4a8.6 8.6 0 1 0 11.2 11.2Z"/> : <><circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 1 0 16Z" fill="currentColor" stroke="none"/></>}
  </svg>;
}

export default function SettingsMenu(props: Props) {
  const { t, preferences, certificate, settings, running, busy, preferencesBusy, ready, native, onPreference, onListenPort, onCopyPac, onAction } = props;
  const [open, setOpen] = useState(false);
  const [subOpen, setSubOpen] = useState(false);
  const [subKind, setSubKind] = useState<'certificate' | 'cache'>('certificate');
  const cacheTrigger = useRef<HTMLButtonElement>(null);
  const activeTrigger = () => subKind === 'cache' ? cacheTrigger.current : certificateTrigger.current;
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const [subPosition, setSubPosition] = useState({ left: 0, top: 0, side: 'left' });
  const wrapper = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const subPanel = useRef<HTMLDivElement>(null);
  const certificateTrigger = useRef<HTMLButtonElement>(null);
  const focusSub = useRef(false);
  const focusParent = useRef(false);
  const previousInline = useRef(false);
  const inlineSub = subOpen && subPosition.side === 'inside';
  const subTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const cancelSubClose = () => clearTimeout(subTimer.current);
  const scheduleSubClose = () => { cancelSubClose(); subTimer.current = setTimeout(() => setSubOpen(false), 180); };
  useEffect(() => () => clearTimeout(subTimer.current), []);
  const disabled = !native || !ready || busy;
  const certificateLabel: MessageKey = certificate.trusted ? 'trusted' : certificate.exists ? 'untrusted' : 'missing';
  const close = () => { cancelSubClose(); focusSub.current = false; focusParent.current = false; setOpen(false); setSubOpen(false); trigger.current?.focus(); };
  const back = () => { cancelSubClose(); focusParent.current = true; setSubOpen(false); };
  function openSub(keyboard = false, kind: 'certificate' | 'cache' = 'certificate') {
    setSubKind(kind); cancelSubClose(); focusSub.current = keyboard;
    if (subOpen && keyboard && kind === subKind) { subPanel.current?.querySelector<HTMLButtonElement>('button:not(:disabled),input[type=checkbox]:not(:disabled)')?.focus(); focusSub.current = false; }
    setSubOpen(true);
  }
  function hoverSub(kind: 'certificate' | 'cache') {
    cancelSubClose();
    const rect = panel.current?.getBoundingClientRect();
    if (rect && (rect.left >= rect.width + 12 || rect.right + rect.width + 12 <= innerWidth)) openSub(false, kind);
  }
  const act = (action: MenuAction) => { close(); onAction(action); };

  useLayoutEffect(() => {
    if (!open) return;
    function place() {
      const anchor = trigger.current!.getBoundingClientRect();
      const box = panel.current!.getBoundingClientRect();
      const next = { left: Math.max(8, Math.min(anchor.right - box.width, innerWidth - box.width - 8)), top: Math.max(8, Math.min(anchor.bottom + 6, innerHeight - box.height - 8)) };
      setPosition(previous => previous.left === next.left && previous.top === next.top ? previous : next);
    }
    place(); window.addEventListener('resize', place); window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, preferences.language]);
  useLayoutEffect(() => {
    if (!open || !subOpen) return;
    function place() {
      const parent = panel.current!.getBoundingClientRect();
      const anchor = activeTrigger()!.getBoundingClientRect();
      const box = subPanel.current!.getBoundingClientRect();
      const right = parent.right + 4 + box.width <= innerWidth - 8;
      const left = parent.left - 4 - box.width >= 8;
      const side = right ? 'right' : left ? 'left' : 'inside';
      const next = {
        left: Math.max(8, Math.min(side === 'inside' ? parent.left : right ? parent.right + 4 : parent.left - box.width - 4, innerWidth - box.width - 8)),
        top: Math.max(8, Math.min(side === 'inside' ? parent.top : anchor.top - 4, innerHeight - box.height - 8)), side,
      };
      setSubPosition(previous => previous.left === next.left && previous.top === next.top && previous.side === next.side ? previous : next);
    }
    place(); window.addEventListener('resize', place); window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, subOpen, position, subPosition.side, subKind, preferences.language]);
  useLayoutEffect(() => {
    if (open) panel.current?.querySelector<HTMLButtonElement>('button:not(:disabled),input[type=checkbox]:not(:disabled)')?.focus();
  }, [open]);
  useLayoutEffect(() => {
    const leavingInline = previousInline.current && !inlineSub;
    previousInline.current = inlineSub;
    if (open && subOpen && (focusSub.current || inlineSub || leavingInline) && !subPanel.current?.contains(document.activeElement)) {
      const target = subPanel.current?.querySelector<HTMLButtonElement>('button:not(:disabled),input[type=checkbox]:not(:disabled)');
      target?.focus();
      if (target) focusSub.current = false;
    }
    if (open && !subOpen && focusParent.current) { activeTrigger()?.focus(); focusParent.current = false; }
  }, [open, subOpen, inlineSub, subKind]);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => { if (!wrapper.current?.contains(event.target as Node)) close(); };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);

  function keys(event: KeyboardEvent<HTMLDivElement>, sub = false) {
    const target = event.target as HTMLElement;
    if (sub && target.matches('input:not([type=checkbox])') && event.key !== 'Escape') return;
    const buttons = Array.from((sub ? subPanel.current : panel.current)?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled)') ?? []);
    if (event.key === 'Escape' || (sub && event.key === 'ArrowLeft')) {
      event.preventDefault(); event.stopPropagation();
      if (subOpen) back(); else close();
      return;
    }
    if (event.key === 'Tab') {
      if ((event.target as HTMLElement).matches('input')) return;
      close(); return;
    }
    if (!sub && (target === certificateTrigger.current || target === cacheTrigger.current) && event.key === 'ArrowRight') {
      event.preventDefault(); openSub(true, target === cacheTrigger.current ? 'cache' : 'certificate');
      return;
    }
    let next: HTMLElement | undefined;
    if (event.key === 'Home') next = buttons[0];
    else if (event.key === 'End') next = buttons.at(-1);
    else if (sub && ['ArrowDown', 'ArrowUp'].includes(event.key)) {
      next = buttons[(buttons.indexOf(target) + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length];
    } else if (!sub && ['ArrowDown', 'ArrowUp'].includes(event.key)) {
      const rows = [...new Set(buttons.map(button => button.dataset.row))];
      const row = rows[(rows.indexOf(target.dataset.row) + (event.key === 'ArrowDown' ? 1 : -1) + rows.length) % rows.length];
      const group = buttons.filter(button => button.dataset.row === row);
      next = group.find(button => button.getAttribute('aria-checked') === 'true') ?? group[0];
    } else if (!sub && ['ArrowLeft', 'ArrowRight'].includes(event.key)) {
      const group = buttons.filter(button => button.dataset.row === target.dataset.row);
      next = group[(group.indexOf(target) + (event.key === 'ArrowRight' ? 1 : -1) + group.length) % group.length];
    }
    if (next) { event.preventDefault(); event.stopPropagation(); next.focus(); if (!sub && next !== certificateTrigger.current && next !== cacheTrigger.current) setSubOpen(false); }
  }
  const item = (row: number, label: MessageKey, action: MenuAction, unavailable = disabled) => <button type="button" role="menuitem" tabIndex={-1} data-row={row} className="menu-item" disabled={unavailable} onPointerEnter={scheduleSubClose} onClick={() => act(action)}>{t(label)}</button>;
  const child = (label: MessageKey, action: MenuAction, unavailable: boolean) => <button type="button" role="menuitem" tabIndex={-1} className="menu-item" data-action={action} disabled={unavailable} onClick={() => act(action)}>{t(label)}</button>;

  return <div className="settings-menu" ref={wrapper}>
    <button type="button" ref={trigger} className="menu-trigger" aria-label={t('menu')} aria-haspopup="menu" aria-expanded={open} aria-controls="settings-menu-panel" onClick={() => { if (open) close(); else setOpen(true); }} onKeyDown={event => { if (event.key === 'ArrowDown') { event.preventDefault(); setOpen(true); } }}>
      <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16"/></svg>
    </button>
    {open && <div ref={panel} id="settings-menu-panel" className="menu-panel" role="menu" aria-label={t('menu')} aria-hidden={inlineSub || undefined} inert={inlineSub} style={position} onKeyDown={event => keys(event)}>
      <button type="button" ref={certificateTrigger} role="menuitem" tabIndex={-1} data-row={0} className="menu-item" aria-haspopup="menu" aria-expanded={subOpen && subKind === 'certificate'} aria-controls="certificate-menu" onPointerEnter={() => hoverSub('certificate')} onClick={() => openSub(true)}><span>{t('manageCertificate')}</span><span aria-hidden="true">›</span></button>
      <button type="button" ref={cacheTrigger} role="menuitem" tabIndex={-1} data-row={1} data-action="cache-menu" className="menu-item" aria-haspopup="menu" aria-expanded={subOpen && subKind === 'cache'} aria-controls="cache-menu" onPointerEnter={() => hoverSub('cache')} onClick={() => openSub(true, 'cache')}><span>{t('manageCache')}</span><span aria-hidden="true">›</span></button>
      {item(2, 'openDirectory', 'data')}
      <div className="menu-separator" role="separator"/>
      <label className="menu-port"><span>{t('localPort')}</span><input data-row={3} aria-label={t('localPort')} type="number" min="1024" max="65535" value={settings.listenPort} disabled={disabled || running} onChange={event => onListenPort(Number(event.target.value))} onBlur={props.onCommitListenPort} onKeyDown={e=>{if(e.key==='Enter'){e.preventDefault();props.onCommitListenPort();}}}/></label>
      <button type="button" role="menuitem" tabIndex={-1} data-row={4} className="menu-item menu-pac" aria-label={t('copyPac')} disabled={disabled} onPointerEnter={scheduleSubClose} onClick={onCopyPac}><span>{t('copyPac')}</span></button>
      <div className="menu-separator" role="separator"/>
      <div className="menu-options language-options" role="group" aria-label={t('language')} onPointerEnter={scheduleSubClose}>
        {(['zh-CN', 'zh-TW', 'ja', 'en'] as const).map((language, index) => <button type="button" role="menuitemradio" data-row={5} tabIndex={-1} aria-checked={preferences.language === language} aria-label={['简体', '繁體', '日本語', 'English'][index]} lang={language} key={language} aria-disabled={preferencesBusy || !ready || props.quitting} onClick={() => { if (!preferencesBusy && ready && !props.quitting) void onPreference({ ...preferences, language }); }}>{['简体', '繁體', '日本語', 'English'][index]}</button>)}
      </div>
      <div className="menu-options theme-options" role="group" aria-label={t('appearance')} onPointerEnter={scheduleSubClose}>
        {(['light', 'dark', 'auto'] as const).map(theme => <button type="button" role="menuitemradio" data-row={6} tabIndex={-1} aria-checked={preferences.theme === theme} aria-label={t(theme)} key={theme} aria-disabled={preferencesBusy || !ready || props.quitting} onClick={() => { if (!preferencesBusy && ready && !props.quitting) void onPreference({ ...preferences, theme }); }}><ThemeIcon theme={theme}/></button>)}
      </div>
      <div className="menu-separator" role="separator"/>
      <button type="button" role="menuitem" tabIndex={-1} data-row={7} data-action="quit" className="menu-item" disabled={!native||props.quitting} onClick={()=>act('quit')}>{t('quit')}</button>
    </div>}
    {open && subOpen && <div ref={subPanel} id={subKind === 'certificate' ? 'certificate-menu' : 'cache-menu'} className="menu-panel certificate-menu" role="menu" aria-label={t(subKind === 'certificate' ? 'manageCertificate' : 'manageCache')} data-side={subPosition.side} onPointerEnter={cancelSubClose} style={{ left: subPosition.left, top: subPosition.top }} onKeyDown={event => keys(event, true)}>
      {inlineSub && <><button type="button" role="menuitem" tabIndex={-1} className="menu-item menu-back" aria-label={t('backToMenu')} onClick={back}><span aria-hidden="true">‹</span><span>{t(subKind === 'certificate' ? 'manageCertificate' : 'manageCache')}</span></button><div className="menu-separator" role="separator"/></>}
      {subKind === 'certificate' ? <>
      <div role="menuitem" aria-disabled="true" className="menu-status">{t('certificate')} · {t(certificateLabel)}</div>
      <div className="menu-separator" role="separator"/>
      {child('installTrust', 'install', disabled || running || certificate.trusted)}
      {child('openCertificate', 'openCertificate', disabled || !certificate.exists)}
      {child('recheck', 'check', disabled || running)}
      {child('removeCertificate', 'remove', disabled || running || !certificate.exists)}
      </> : <>
        <div role="menuitem" aria-disabled="true" className="menu-status cache-usage">{t('used')} <span className="numeric">{props.cacheUsage}</span></div>
        <label className="menu-port menu-cache-limit"><span>{t('limit')}</span><input aria-label={t('limit')} type="number" min="1" max="100" value={settings.cacheLimitGb} disabled={disabled || running} onChange={event => props.onCacheLimit(Number(event.target.value))} onBlur={props.onCommitCacheLimit} onKeyDown={e=>{if(e.key==='Enter'){e.preventDefault();props.onCommitCacheLimit();}}}/><span>GB</span></label>
        <div className="menu-separator" role="separator"/>
        {(['prefetchEnabled', 'warmupEnabled'] as const).map(key => <label key={key} className="menu-item checkbox"><input type="checkbox" role="menuitemcheckbox" aria-label={t(key)} checked={props.cachePreferences[key]} disabled={disabled || props.auditRunning || props.cacheBusy} aria-busy={props.cacheBusy || undefined} onChange={event => void props.onCachePreference({ [key]: event.target.checked })}/><span>{t(key)}</span></label>)}
        <div className="menu-separator" role="separator"/>
        {child('auditTitle', 'audit', disabled || running || props.auditRunning)}
        {child('clearCacheMenu', 'clear', disabled || running || props.auditRunning)}
      </>}
    </div>}
  </div>;
}
