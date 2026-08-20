/**
 * Lightweight i18n for Lyra.
 *
 * v1 supports English (en) and Chinese (zh).
 * Translations are loaded eagerly; locale is stored in the UI Zustand store.
 *
 * For v1 we keep this simple — no lazy loading, no ICU message format.
 * If complexity grows, migrate to a full i18n library (e.g. i18next).
 */

import en from './en.json';
import zh from './zh.json';
import type { SupportedLocale } from '../types';

type TranslationDict = Record<string, Record<string, string>>;

const translations: Record<SupportedLocale, TranslationDict> = { en, zh };

/**
 * Get a nested translation value by dot-separated key.
 * e.g. t("mail.compose") → "Compose" (en) / "写邮件" (zh)
 */
export function t(locale: SupportedLocale, key: string): string {
  const parts = key.split('.');
  let current: Record<string, unknown> = translations[locale];

  for (const part of parts) {
    if (current && typeof current === 'object' && part in current) {
      current = current[part] as Record<string, unknown>;
    } else {
      // Fallback to English
      current = translations.en;
      for (const fallbackPart of parts) {
        if (current && typeof current === 'object' && fallbackPart in current) {
          current = current[fallbackPart] as Record<string, unknown>;
        } else {
          return key; // Key not found
        }
      }
      return typeof current === 'string' ? current : key;
    }
  }

  return typeof current === 'string' ? current : key;
}

export type { SupportedLocale };
