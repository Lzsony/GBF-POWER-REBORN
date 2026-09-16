import traditional from './locales/zh-TW.json' with { type: 'json' };
import simplified from './locales/zh-CN.json' with { type: 'json' };
import type { Language } from './types';
export type MessageKey = keyof typeof traditional;
export type Message = { key: MessageKey; args?: Record<string, string | number> };
export const dictionaries: Record<Language, Record<MessageKey, string>> = {
  'zh-TW': traditional, 'zh-CN': simplified,
};
export function translate(language: Language, key: MessageKey, args: Record<string, string | number> = {}) {
  return dictionaries[language][key].replace(/\{(\w+)\}/g, (_, name: string) => String(args[name] ?? `{${name}}`));
}
export function errorMessage(error: unknown): MessageKey {
  const code = error && typeof error === 'object' && 'code' in error ? String(error.code) : 'INTERNAL_FAILURE';
  const key = `error.${code}`;
  return key in traditional ? key as MessageKey : 'error.INTERNAL_FAILURE';
}
export function languageFromSystem(locale: string): Language {
  const value = locale.toLowerCase().replaceAll('_', '-');
  if (!value.startsWith('zh')) return 'zh-TW';
  if (value.includes('hant')) return 'zh-TW';
  if (value.includes('hans')) return 'zh-CN';
  return value.split('-').some(v => ['tw', 'hk', 'mo'].includes(v)) ? 'zh-TW' : 'zh-CN';
}
