import { describe, expect, it } from 'vitest';

import { dkimSummary } from '@/lib/dkim';
import type { DkimInfo } from '@/types';

const base: DkimInfo = {
  status: 'pass',
  sdid: 'duck.com',
  auid: '@duck.com',
  selector: 'dkim',
  algorithm: 'RsaSha256',
  signedHeaders: ['date', 'from', 'to'],
  warnings: [],
  signedAt: null,
  expiresAt: null,
};

describe('dkimSummary', () => {
  it('pass names the signing domain', () => {
    expect(dkimSummary('en', base)).toBe('DKIM Valid (Signed by duck.com)');
    expect(dkimSummary('zh', base)).toBe('DKIM 验证通过（签名方 duck.com）');
  });

  it('fail reports modification', () => {
    const v = { ...base, status: 'fail' as const };
    expect(dkimSummary('en', v)).toBe('DKIM Invalid (E-Mail was modified)');
    expect(dkimSummary('zh', v)).toBe('DKIM 无效（邮件已被修改）');
  });

  it('none and temperror are neutral', () => {
    expect(dkimSummary('en', { ...base, status: 'none' })).toBe('Not signed');
    expect(dkimSummary('en', { ...base, status: 'temperror' })).toBe('Not signed');
    expect(dkimSummary('zh', { ...base, status: 'none' })).toBe('未签名');
  });

  it('pass without sdid falls back gracefully', () => {
    expect(dkimSummary('en', { ...base, sdid: null })).toBe('DKIM Valid');
    expect(dkimSummary('zh', { ...base, sdid: null })).toBe('DKIM 验证通过');
  });
});
