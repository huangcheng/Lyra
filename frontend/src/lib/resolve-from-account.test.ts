import { describe, expect, it } from 'vitest';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { resolveFromAccountId } from '@/lib/resolve-from-account';
import type { MailAccount } from '@/types';

function account(id: string): MailAccount {
  return {
    id,
    displayName: id,
    emailAddress: `${id}@example.com`,
    protocol: 'imap',
    isActive: true,
    syncEnabled: true,
  };
}

const accounts = [account('a'), account('b'), account('c')];

describe('resolveFromAccountId', () => {
  it('prefers the draft source account (reply/forward/edit)', () => {
    expect(
      resolveFromAccountId({
        draftAccountId: 'c',
        selectedAccountId: 'b',
        defaultAccountId: 'a',
        accounts,
      }),
    ).toBe('c');
  });

  it('uses the browsed account when not in unified view', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: 'b',
        defaultAccountId: 'a',
        accounts,
      }),
    ).toBe('b');
  });

  it('uses the default account in unified view', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: 'b',
        accounts,
      }),
    ).toBe('b');
  });

  it('falls back to the first account when no default is set', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: null,
        accounts,
      }),
    ).toBe('a');
  });

  it('falls back when the default account was deleted', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: 'gone',
        accounts,
      }),
    ).toBe('a');
  });

  it('falls back when the draft account is unknown', () => {
    expect(
      resolveFromAccountId({
        draftAccountId: 'gone',
        selectedAccountId: 'b',
        defaultAccountId: null,
        accounts,
      }),
    ).toBe('b');
  });

  it('falls back when the browsed account is stale', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: 'gone',
        defaultAccountId: 'b',
        accounts,
      }),
    ).toBe('b');
  });

  it('returns empty string with no accounts', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: null,
        accounts: [],
      }),
    ).toBe('');
  });
});
