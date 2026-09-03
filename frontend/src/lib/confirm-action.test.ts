import { afterEach, describe, expect, it } from 'vitest';

import { confirmAction, registerConfirmHost, type ConfirmPending } from './confirm-action';

describe('confirmAction', () => {
  afterEach(() => {
    registerConfirmHost(null);
  });

  it('resolves false when no host is registered', async () => {
    await expect(confirmAction({ title: 'Delete?' })).resolves.toBe(false);
  });

  it('resolves true when the host settles confirm', async () => {
    const holder: { pending: ConfirmPending | null } = { pending: null };
    registerConfirmHost((next) => {
      holder.pending = next;
    });

    const result = confirmAction({ title: 'Move to trash?', tone: 'destructive' });
    expect(holder.pending).not.toBeNull();
    expect(holder.pending!.title).toBe('Move to trash?');
    expect(holder.pending!.tone).toBe('destructive');
    holder.pending!.resolve(true);

    await expect(result).resolves.toBe(true);
  });

  it('resolves false when the host settles cancel', async () => {
    const holder: { pending: ConfirmPending | null } = { pending: null };
    registerConfirmHost((next) => {
      holder.pending = next;
    });

    const result = confirmAction({ title: 'Delete account?' });
    holder.pending!.resolve(false);
    await expect(result).resolves.toBe(false);
  });
});
