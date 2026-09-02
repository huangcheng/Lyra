import { afterEach, describe, expect, it, vi } from 'vitest';

import { confirmMoveToTrash } from './confirm-trash';

describe('confirmMoveToTrash', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('asks once for a single message and returns the confirm result', () => {
    const confirm = vi.fn(() => true);
    vi.stubGlobal('confirm', confirm);
    expect(confirmMoveToTrash('en')).toBe(true);
    expect(confirm).toHaveBeenCalledWith('Move this message to trash?');
  });

  it('uses the plural copy when count > 1', () => {
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    expect(confirmMoveToTrash('zh', 3)).toBe(false);
    expect(confirm).toHaveBeenCalledWith('将 3 封邮件移到废纸篓？');
  });
});
