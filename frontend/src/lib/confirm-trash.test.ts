import { afterEach, describe, expect, it } from 'vitest';

import { registerConfirmHost, type ConfirmPending } from './confirm-action';
import { confirmMoveToTrash } from './confirm-trash';

describe('confirmMoveToTrash', () => {
  afterEach(() => {
    registerConfirmHost(null);
  });

  it('asks once for a single message and returns the confirm result', async () => {
    const holder: { pending: ConfirmPending | null } = { pending: null };
    registerConfirmHost((next) => {
      holder.pending = next;
    });

    const result = confirmMoveToTrash('en');
    expect(holder.pending?.title).toBe('Move this message to trash?');
    expect(holder.pending?.tone).toBe('destructive');
    holder.pending!.resolve(true);
    await expect(result).resolves.toBe(true);
  });

  it('uses the plural copy when count > 1', async () => {
    const holder: { pending: ConfirmPending | null } = { pending: null };
    registerConfirmHost((next) => {
      holder.pending = next;
    });

    const result = confirmMoveToTrash('zh', 3);
    expect(holder.pending?.title).toBe('将 3 封邮件移到废纸篓？');
    holder.pending!.resolve(false);
    await expect(result).resolves.toBe(false);
  });
});
