/**
 * i18n dictionary parity: en.json and zh.json must expose the identical
 * nested key structure — a missing translation silently falls back to
 * English via t(), which is how mixed-language UIs sneak in.
 */

import { describe, expect, it } from 'vitest';

import en from './en.json';
import zh from './zh.json';
import { t } from './index';

type Dict = Record<string, unknown>;

/** All leaf key paths of a nested dict, dot-separated and sorted. */
function leafPaths(dict: Dict, prefix = ''): string[] {
  return Object.keys(dict)
    .sort()
    .flatMap((key) => {
      const path = prefix ? `${prefix}.${key}` : key;
      const value = dict[key];
      if (value && typeof value === 'object' && !Array.isArray(value)) {
        return leafPaths(value as Dict, path);
      }
      return [path];
    });
}

describe('i18n dictionaries', () => {
  it('en and zh have identical nested key structure', () => {
    expect(leafPaths(zh as Dict)).toEqual(leafPaths(en as Dict));
  });

  it('every zh leaf is a non-empty string', () => {
    const check = (dict: Dict) => {
      for (const value of Object.values(dict)) {
        if (value && typeof value === 'object') check(value as Dict);
        else {
          expect(typeof value).toBe('string');
          expect((value as string).trim().length).toBeGreaterThan(0);
        }
      }
    };
    check(zh as Dict);
  });

  it('t() resolves keys in both locales', () => {
    expect(t('en', 'mail.compose')).toBe('Compose');
    expect(t('zh', 'mail.compose')).not.toBe('Compose');
    expect(t('zh', 'mail.to')).toBe('收件人');
  });

  it('t() interpolates {{params}}', () => {
    expect(t('en', 'mail.replyPlaceholder', { name: 'Ada' })).toContain('Ada');
    expect(t('zh', 'mail.replyPlaceholder', { name: 'Ada' })).toContain('Ada');
  });
});
