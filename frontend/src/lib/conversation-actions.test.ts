import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ api: vi.fn().mockResolvedValue({}) }));

import { api } from '@/lib/api-client';
import {
  actOnMessages,
  canDropConversation,
  moveMessages,
  patchMessages,
} from '@/lib/conversation-actions';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

const mockedApi = vi.mocked(api);

function msg(id: string, over: Partial<MailMessage> = {}): MailMessage {
  return {
    id,
    accountId: 'acc1',
    folderId: 'f1',
    subject: `S${id}`,
    from: { email: 'a@example.com' },
    to: [],
    date: '2026-09-01T10:00:00Z',
    snippet: '',
    isRead: false,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
    ...over,
  };
}

beforeEach(() => {
  mockedApi.mockClear();
  mockedApi.mockResolvedValue({});
  useMailStore.setState({
    messages: { m1: msg('m1'), m2: msg('m2'), m3: msg('m3') },
  });
  useUIStore.setState({ selectedMessageId: null });
});

describe('moveMessages', () => {
  it('moves each message and removes it locally', async () => {
    const res = await moveMessages(['m1', 'm2'], 'f2');
    expect(res.error).toBeNull();
    expect(res.done).toEqual(['m1', 'm2']);
    expect(mockedApi).toHaveBeenCalledTimes(2);
    expect(mockedApi).toHaveBeenCalledWith('/messages/m1/move', {
      method: 'POST',
      body: JSON.stringify({ folderId: 'f2' }),
    });
    expect(useMailStore.getState().messages.m1).toBeUndefined();
    expect(useMailStore.getState().messages.m3).toBeDefined();
  });

  it('stops at the first failure and reports it', async () => {
    mockedApi.mockResolvedValueOnce({}).mockRejectedValueOnce(new Error('IMAP MOVE failed'));
    const res = await moveMessages(['m1', 'm2', 'm3'], 'f2');
    expect(res.done).toEqual(['m1']);
    expect(res.error).toBe('IMAP MOVE failed');
    expect(mockedApi).toHaveBeenCalledTimes(2);
    expect(useMailStore.getState().messages.m2).toBeDefined();
  });

  it('clears the selection when the selected message is moved', async () => {
    useUIStore.setState({ selectedMessageId: 'm1' });
    await moveMessages(['m1'], 'f2');
    expect(useUIStore.getState().selectedMessageId).toBeNull();
  });
});

describe('actOnMessages', () => {
  it('posts the action per message and removes locally', async () => {
    const res = await actOnMessages(['m1', 'm2'], 'archive');
    expect(res.error).toBeNull();
    expect(mockedApi).toHaveBeenCalledWith('/messages/m2/archive', { method: 'POST' });
    expect(useMailStore.getState().messages.m1).toBeUndefined();
  });
});

describe('patchMessages', () => {
  it('marks read locally after the patch', async () => {
    await patchMessages(['m1'], { isRead: true });
    expect(mockedApi).toHaveBeenCalledWith('/messages/m1', {
      method: 'PATCH',
      body: JSON.stringify({ isRead: true }),
    });
    expect(useMailStore.getState().messages.m1.isRead).toBe(true);
  });

  it('only toggles star when the local state differs', async () => {
    useMailStore.setState({ messages: { m1: msg('m1', { isStarred: true }) } });
    await patchMessages(['m1'], { isStarred: true });
    expect(useMailStore.getState().messages.m1.isStarred).toBe(true);
  });
});

describe('canDropConversation', () => {
  const drag = { accountId: 'acc1', folderIds: ['f1'] };
  it('rejects cross-account drops', () => {
    expect(canDropConversation(drag, { accountId: 'acc2', folderId: 'f9' })).toBe(false);
  });
  it('rejects dropping into the current folder', () => {
    expect(canDropConversation(drag, { accountId: 'acc1', folderId: 'f1' })).toBe(false);
  });
  it('accepts a same-account different folder', () => {
    expect(canDropConversation(drag, { accountId: 'acc1', folderId: 'f2' })).toBe(true);
  });
});
