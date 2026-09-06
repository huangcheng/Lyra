import { describe, expect, it } from 'vitest';

import { normalizeDavUrlInput, pimProviderFor, pimSetupStatus, providerHintKey } from './pim-setup';

describe('pimProviderFor', () => {
  it('maps known provider domains', () => {
    expect(pimProviderFor('huangcheng@fastmail.com')).toBe('fastmail');
    expect(pimProviderFor('a@YANDEX.RU')).toBe('yandex');
    expect(pimProviderFor('b@mail.icloud.com')).toBe('icloud');
    expect(pimProviderFor('c@gmail.com')).toBe('google');
    expect(pimProviderFor('d@outlook.com')).toBe('microsoft');
    expect(pimProviderFor('e@live.in')).toBe('microsoft');
  });

  it('matches subdomains of provider hosts', () => {
    expect(pimProviderFor('x@mail.fastmail.com')).toBe('fastmail');
    expect(pimProviderFor('x@sub.outlook.com')).toBe('microsoft');
  });

  it('falls back to generic for unknown or malformed addresses', () => {
    expect(pimProviderFor('z@example.com')).toBe('generic');
    expect(pimProviderFor('hosted@corp.example.net')).toBe('generic');
    expect(pimProviderFor('no-at-sign')).toBe('generic');
    expect(pimProviderFor('')).toBe('generic');
  });
});

describe('pimSetupStatus', () => {
  const base = { hasCredential: false, carddavUrl: null, caldavUrl: null };

  it('is off with nothing configured', () => {
    expect(pimSetupStatus(base)).toBe('off');
    expect(pimSetupStatus({ ...base, carddavUrl: '', caldavUrl: '' })).toBe('off');
  });

  it('detects partial setups', () => {
    expect(pimSetupStatus({ ...base, carddavUrl: 'https://dav.example.com/' })).toBe(
      'contactsOnly',
    );
    expect(pimSetupStatus({ ...base, caldavUrl: 'https://dav.example.com/' })).toBe(
      'calendarsOnly',
    );
    expect(pimSetupStatus({ ...base, hasCredential: true })).toBe('credentialOnly');
  });

  it('is connected when both homesets are known', () => {
    expect(
      pimSetupStatus({
        hasCredential: true,
        carddavUrl: 'https://carddav.example.com/dav/',
        caldavUrl: 'https://caldav.example.com/dav/',
      }),
    ).toBe('connected');
    // Credential-less but discovered still counts as connected: the DAV
    // password falls back to the mail credential server-side.
    expect(
      pimSetupStatus({
        hasCredential: false,
        carddavUrl: 'https://carddav.example.com/dav/',
        caldavUrl: 'https://caldav.example.com/dav/',
      }),
    ).toBe('connected');
  });
});

describe('providerHintKey', () => {
  it('returns one i18n key per provider', () => {
    expect(providerHintKey('fastmail')).toBe('settings.pim.hintFastmail');
    expect(providerHintKey('microsoft')).toBe('settings.pim.hintMicrosoft');
    expect(providerHintKey('generic')).toBe('settings.pim.hintGeneric');
  });
});

describe('normalizeDavUrlInput', () => {
  it('accepts empty input as an explicit clear', () => {
    expect(normalizeDavUrlInput('')).toEqual({ ok: true, value: '' });
    expect(normalizeDavUrlInput('   ')).toEqual({ ok: true, value: '' });
  });

  it('trims valid http(s) URLs', () => {
    expect(normalizeDavUrlInput('  https://dav.example.com/dav/ ')).toEqual({
      ok: true,
      value: 'https://dav.example.com/dav/',
    });
    expect(normalizeDavUrlInput('http://localhost:8080/remote.php/dav/')).toEqual({
      ok: true,
      value: 'http://localhost:8080/remote.php/dav/',
    });
  });

  it('rejects values that are not absolute http(s) URLs', () => {
    // The backend rejects public http hosts; the client only checks shape —
    // scheme policy is the server's call (it knows about LAN exceptions).
    expect(normalizeDavUrlInput('dav.example.com')).toEqual({
      ok: false,
      errorKey: 'settings.pim.urlInvalid',
    });
    expect(normalizeDavUrlInput('ftp://dav.example.com/')).toEqual({
      ok: false,
      errorKey: 'settings.pim.urlInvalid',
    });
    expect(normalizeDavUrlInput('https://')).toEqual({
      ok: false,
      errorKey: 'settings.pim.urlInvalid',
    });
  });
});
