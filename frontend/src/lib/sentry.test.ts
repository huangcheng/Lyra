import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('@sentry/react', () => ({
  init: vi.fn(),
}));

import { init } from '@sentry/react';
import { initSentryFromVersion, shouldInitSentry } from '@/lib/sentry';

const mockedInit = vi.mocked(init);

beforeEach(() => {
  mockedInit.mockClear();
});

describe('shouldInitSentry', () => {
  it('accepts well-formed https DSNs', () => {
    expect(shouldInitSentry('https://abc@o1.ingest.sentry.io/1')).toBe(true);
  });

  it('rejects missing, malformed, or non-https DSNs', () => {
    expect(shouldInitSentry(undefined)).toBe(false);
    expect(shouldInitSentry(null)).toBe(false);
    expect(shouldInitSentry('')).toBe(false);
    expect(shouldInitSentry('not-a-dsn')).toBe(false);
    expect(shouldInitSentry('http://abc@o1.ingest.sentry.io/1')).toBe(false);
  });
});

describe('initSentryFromVersion', () => {
  it('initializes with the advertised DSN and release tag', () => {
    initSentryFromVersion({ version: '0.2.0', sentryDsn: 'https://abc@o1.ingest.sentry.io/1' });
    expect(mockedInit).toHaveBeenCalledTimes(1);
    expect(mockedInit).toHaveBeenCalledWith(
      expect.objectContaining({
        dsn: 'https://abc@o1.ingest.sentry.io/1',
        release: 'lyra-web@0.2.0',
      }),
    );
  });

  it('stays a no-op without a DSN — nothing ever initializes', () => {
    initSentryFromVersion({ version: '0.2.0' });
    initSentryFromVersion({ version: '0.2.0', sentryDsn: null });
    initSentryFromVersion(null);
    expect(mockedInit).not.toHaveBeenCalled();
  });
});
