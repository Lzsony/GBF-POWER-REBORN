import { useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import type { Settings } from './types';
import { errorMessage, type MessageKey } from './i18n';

/** Keep UI drafts separate from both redacted status snapshots and asynchronous secure reads. */
export function useProxyUrl(settings: Settings, ready: boolean, reportError: (key: MessageKey | null) => void) {
  const [revision,setRevision]=useState(0);
  const [value, setValue] = useState('');
  const [edited, setEdited] = useState(false);
  const [loading, setLoading] = useState(false);
  const [resolved, setResolved] = useState(false);
  const editedRef = useRef(false);
  const loadedFor = useRef<string | null>(null);
  const source = `${settings.proxyUrl}\0${settings.hasAuthentication}`;

  useEffect(() => {
    if (!ready || settings.mode !== 'proxy' || editedRef.current || loadedFor.current === source) return;
    let cancelled = false;
    setResolved(false); setValue('');
    if (!settings.hasAuthentication) {
      setValue(settings.proxyUrl); loadedFor.current = source; setResolved(true); setLoading(false);
      return;
    }
    setLoading(true);
    const read = isTauri() ? invoke<string>('reveal_proxy_url') : Promise.reject({ code: 'SECRET_READ_FAILED' });
    void read.then(url => {
      if (cancelled || editedRef.current) return;
      setValue(url); loadedFor.current = source; setResolved(true);
    }).catch(error => {
      if (cancelled || editedRef.current) return;
      reportError(errorMessage(error)); setValue('');
    }).finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [ready, settings.mode, settings.proxyUrl, settings.hasAuthentication, source, reportError,revision]);

  function change(next: string) {
    editedRef.current = true; setEdited(true); setValue(next); setLoading(false); reportError(null);
  }
  function saved(next: Settings) {
    // Applying Direct changes no proxy credentials. Keep its hidden, unsaved URL draft.
    if (next.mode !== 'proxy') return;
    loadedFor.current = `${next.proxyUrl}\0${next.hasAuthentication}`;
    editedRef.current = false; setEdited(false); setResolved(true); setLoading(false);
  }
  function reset(){editedRef.current=false;setEdited(false);loadedFor.current=null;setRevision(v=>v+1);}
  return { reset,value, change, saved, edited, loading, unavailable: settings.mode === 'proxy' && !edited && !resolved };
}
