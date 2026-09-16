export type Language = 'zh-CN' | 'zh-TW';
export type Preferences = { theme: 'light' | 'dark' | 'auto'; language: Language };
export type Settings = {
  mode: 'direct' | 'proxy'; proxyUrl: string; hasAuthentication: boolean;
  listenPort: number; httpsCache: boolean; cacheLimitGb: number; autostart: boolean;
};
export type CertificateStatus = { exists: boolean; trusted: boolean; fingerprint: string | null };
export type Status = {
  running: boolean; settings: Settings; preferences: Preferences; pacUrl: string; cacheBytes: number;
  certificate: CertificateStatus;
  metrics: { requests: number; downloads: number; hitRate: number | null;
    received: number; sent: number; uptimeSecs: number;
    network: { lineLatencyMs: number | null; lineMeanMs: number | null; lineJitterMs: number | null;
      lineTimeoutPercent: number | null; gameLatencyMs: number | null; gameMinMs:number|null; gameMedianMs:number|null; gameMaxMs:number|null; gameJitterMs: number | null; gameTimeoutPercent: number | null;
      steamLatencyMs: number | null; steamMinMs:number|null; steamMedianMs:number|null; steamMaxMs:number|null; steamJitterMs: number | null; steamTimeoutPercent: number | null } };
};
export const initialSettings: Settings = {
  mode: 'direct', proxyUrl: 'http://127.0.0.1:7890', hasAuthentication: false,
  listenPort: 8123, httpsCache: false, cacheLimitGb: 5, autostart: false,
};
export const initialStatus: Status = {
  running: false, settings: initialSettings, preferences: { theme: 'auto', language: 'zh-TW' },
  pacUrl: 'http://127.0.0.1:8123/proxy.pac', cacheBytes: 0,
  certificate: { exists: false, trusted: false, fingerprint: null },
  metrics: { requests: 0, downloads: 0, hitRate: null, received: 0, sent: 0, uptimeSecs: 0,
    network: { lineMeanMs:null, gameMinMs:null, gameMedianMs:null, gameMaxMs:null, steamMinMs:null, steamMedianMs:null, steamMaxMs:null, lineLatencyMs: null, lineJitterMs: null, lineTimeoutPercent: null,
      gameLatencyMs: null, gameJitterMs: null, gameTimeoutPercent: null, steamLatencyMs: null, steamJitterMs: null, steamTimeoutPercent: null } },
};
