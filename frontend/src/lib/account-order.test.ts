import { describe, expect, it } from 'vitest';

import { moveId, orderAccounts } from '@/lib/account-order';
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

describe('orderAccounts', () => {
  it('returns server order when accountOrder is empty', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, []).map((a) => a.id)).toEqual(['a', 'b', 'c']);
  });

  it('honors the persisted id order', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, ['c', 'a', 'b']).map((a) => a.id)).toEqual(['c', 'a', 'b']);
  });

  it('appends accounts missing from accountOrder in server order', () => {
    const accounts = [account('a'), account('b'), account('c')];
    expect(orderAccounts(accounts, ['b']).map((a) => a.id)).toEqual(['b', 'a', 'c']);
  });

  it('ignores stale ids that match no account', () => {
    const accounts = [account('a'), account('b')];
    expect(orderAccounts(accounts, ['ghost', 'b', 'a']).map((a) => a.id)).toEqual(['b', 'a']);
  });

  it('does not mutate the input array', () => {
    const accounts = [account('a'), account('b')];
    orderAccounts(accounts, ['b', 'a']);
    expect(accounts.map((a) => a.id)).toEqual(['a', 'b']);
  });
});

describe('moveId', () => {
  it('moves an entry to a later position', () => {
    expect(moveId(['a', 'b', 'c', 'd'], 'a', 'c')).toEqual(['b', 'c', 'a', 'd']);
  });

  it('moves an entry to an earlier position', () => {
    expect(moveId(['a', 'b', 'c', 'd'], 'd', 'b')).toEqual(['a', 'd', 'b', 'c']);
  });

  it('moves to the top and to the end', () => {
    expect(moveId(['a', 'b', 'c'], 'c', 'a')).toEqual(['c', 'a', 'b']);
    expect(moveId(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
  });

  it('is a no-op for identical or unknown ids', () => {
    expect(moveId(['a', 'b'], 'a', 'a')).toEqual(['a', 'b']);
    expect(moveId(['a', 'b'], 'x', 'a')).toEqual(['a', 'b']);
    expect(moveId(['a', 'b'], 'a', 'x')).toEqual(['a', 'b']);
  });
});
