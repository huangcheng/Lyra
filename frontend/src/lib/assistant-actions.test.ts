import { describe, expect, it } from 'vitest';

import { describePendingAction } from '@/lib/assistant-actions';

describe('describePendingAction', () => {
  it('describes an open-draft proposal', () => {
    const label = describePendingAction(
      { type: 'openDraft', to: 'a@b.com', subject: 'Re: hi', body: 'x' },
      'zh',
    );
    expect(label).toContain('a@b.com');
  });

  it('localizes move proposals per action', () => {
    expect(
      describePendingAction({ type: 'moveMessage', messageId: 'm1', action: 'spam' }, 'zh'),
    ).toMatch(/垃圾/);
    expect(
      describePendingAction({ type: 'moveMessage', messageId: 'm1', action: 'notSpam' }, 'en'),
    ).toMatch(/not spam/i);
  });
});
