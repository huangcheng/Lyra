import { describe, expect, it } from 'vitest';

import {
  nextAuthTypeOnSecret,
  oauthErrorKey,
  probeFormPatch,
  probeSourceLabel,
} from './account-form';

describe('nextAuthTypeOnSecret', () => {
  it('flips password → bearer when a Fastmail token is pasted on JMAP', () => {
    expect(nextAuthTypeOnSecret('password', 'jmap', 'fmu1-abc')).toBe('bearer');
  });

  it('keeps bearer when the token keeps being edited', () => {
    expect(nextAuthTypeOnSecret('bearer', 'jmap', 'fmu1-abcd')).toBe('bearer');
  });

  it('does not flip while the form is still on IMAP', () => {
    // The flip happens when the probe switches to JMAP instead
    // (probeFormPatch) — pasting early on the IMAP default must not
    // silently change the method the user sees.
    expect(nextAuthTypeOnSecret('password', 'imap', 'fmu1-abc')).toBe('password');
  });

  it('never flips for ordinary passwords', () => {
    expect(nextAuthTypeOnSecret('password', 'jmap', 'hunter2')).toBe('password');
    expect(nextAuthTypeOnSecret('password', 'jmap', 'my-fmu1-password')).toBe('password');
  });
});

describe('probeFormPatch', () => {
  const jmapProbe = { found: true, protocol: 'imap', jmapSupported: true };
  const imapProbe = { found: true, protocol: 'imap', jmapSupported: false };

  it('upgrades a pasted token to bearer when switching the form to JMAP', () => {
    // The production race: token pasted while the form still showed the
    // IMAP default, probe lands late and flips protocol to JMAP — the
    // auth method must follow, or Basic-with-token 401s.
    const patch = probeFormPatch(jmapProbe, {
      protocolTouched: false,
      protocol: 'imap',
      authType: 'password',
      secret: 'fmu1-late-probe',
    });
    expect(patch).toEqual({ protocol: 'jmap', authType: 'bearer' });
  });

  it('switches protocol but keeps an explicitly chosen auth method', () => {
    const patch = probeFormPatch(jmapProbe, {
      protocolTouched: false,
      protocol: 'imap',
      authType: 'bearer',
      secret: 'fmu1-late-probe',
    });
    expect(patch).toEqual({ protocol: 'jmap', authType: 'bearer' });
  });

  it('never overrides a protocol the user picked by hand', () => {
    const patch = probeFormPatch(jmapProbe, {
      protocolTouched: true,
      protocol: 'imap',
      authType: 'password',
      secret: 'app-password',
    });
    expect(patch).toBeNull();
  });

  it('leaves password auth alone for non-token secrets', () => {
    const patch = probeFormPatch(jmapProbe, {
      protocolTouched: false,
      protocol: 'imap',
      authType: 'password',
      secret: 'ordinary-app-password',
    });
    expect(patch).toEqual({ protocol: 'jmap', authType: 'password' });
  });

  it('returns nothing when the probe has no JMAP support', () => {
    expect(
      probeFormPatch(imapProbe, {
        protocolTouched: false,
        protocol: 'imap',
        authType: 'password',
        secret: '',
      }),
    ).toBeNull();
  });
});

describe('probeSourceLabel', () => {
  it('maps every backend slug to a human label', () => {
    expect(probeSourceLabel('en', 'common_patterns')).toBe('built-in provider settings');
    expect(probeSourceLabel('zh', 'common_patterns')).toBe('内置服务商配置');
    expect(probeSourceLabel('en', 'mozilla_ispdb')).toContain('Mozilla');
  });

  it('falls back to the raw value for unknown sources', () => {
    expect(probeSourceLabel('en', 'future_source')).toBe('future_source');
  });

  it('names the missing source instead of rendering undefined', () => {
    expect(probeSourceLabel('en', undefined)).toBe('an unknown source');
    expect(probeSourceLabel('zh', null)).toBe('未知来源');
  });
});

describe('oauthErrorKey', () => {
  it('maps callback detail codes to specific messages', () => {
    expect(oauthErrorKey('oauth_denied')).toBe('settings.accounts.oauthDenied');
    expect(oauthErrorKey('token_exchange')).toBe('settings.accounts.oauthTokenExchange');
    expect(oauthErrorKey('anything_else')).toBe('settings.accounts.oauthError');
    expect(oauthErrorKey(null)).toBe('settings.accounts.oauthError');
  });
});
