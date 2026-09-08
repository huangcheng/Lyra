/**
 * Notification helper unit tests: prefs persistence robustness and the
 * sender-label extraction from the stored `fromAddress` forms.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  isFolderMuted,
  isIncomingFolderRole,
  isThreadMuted,
  messageIdentity,
  readNotificationPrefs,
  senderLabel,
  setFolderMuted,
  setThreadMuted,
  writeNotificationPrefs,
  type NotificationPrefs,
} from './notifications';
import type { ApiMessage } from './mail-api';
import { syncPushPrefs } from './push';

vi.mock('./push', () => ({ syncPushPrefs: vi.fn() }));

function msg(fromAddress?: string): ApiMessage {
  return {
    id: 'm1',
    accountId: 'a1',
    folderId: 'f1',
    isRead: false,
    isStarred: false,
    hasAttachments: false,
    fromAddress,
  };
}

describe('notification prefs', () => {
  afterEach(() => localStorage.clear());

  it('round-trips', () => {
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [], mutedThreadIds: [] });
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: true,
      mutedFolderIds: [],
      mutedThreadIds: [],
    });
  });

  it('defaults when unset or corrupted', () => {
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
      mutedThreadIds: [],
    });
    localStorage.setItem('lyra.notifications', '{not json');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
      mutedThreadIds: [],
    });
    localStorage.setItem('lyra.notifications', '{"enabled":"yes"}');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
      mutedThreadIds: [],
    });
  });

  it('reads legacy blobs without mutedFolderIds and filters junk entries', () => {
    localStorage.setItem('lyra.notifications', '{"enabled":true}');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: true,
      mutedFolderIds: [],
      mutedThreadIds: [],
    });
    localStorage.setItem('lyra.notifications', '{"enabled":true,"mutedFolderIds":["f1",7,"f2"]}');
    expect(readNotificationPrefs().mutedFolderIds).toEqual(['f1', 'f2']);
  });

  it('mutes and unmutes folders', () => {
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [], mutedThreadIds: [] });
    setFolderMuted('f1', true);
    expect(isFolderMuted('f1')).toBe(true);
    expect(isFolderMuted('f2')).toBe(false);
    setFolderMuted('f1', false);
    expect(isFolderMuted('f1')).toBe(false);
    expect(readNotificationPrefs().enabled).toBe(true);
  });

  it('mutes and unmutes threads', () => {
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [], mutedThreadIds: [] });
    setThreadMuted('t1', true);
    expect(isThreadMuted('t1')).toBe(true);
    expect(isThreadMuted('t2')).toBe(false);
    setThreadMuted('t1', false);
    expect(isThreadMuted('t1')).toBe(false);
    expect(readNotificationPrefs().mutedFolderIds).toEqual([]);
  });

  it('writeNotificationPrefs mirrors mutes to the push server', () => {
    writeNotificationPrefs({ enabled: true, mutedFolderIds: ['f1'], mutedThreadIds: ['t1'] });
    expect(syncPushPrefs).toHaveBeenCalledWith({
      mutedFolderIds: ['f1'],
      mutedThreadIds: ['t1'],
      locale: expect.any(String),
    });
  });
});

describe('senderLabel', () => {
  it('prefers the display name from the JSON array form', () => {
    expect(senderLabel(msg('[{"name":"Ada Lovelace","email":"ada@example.com"}]'))).toBe(
      'Ada Lovelace',
    );
  });

  it('falls back to email inside the array entry', () => {
    expect(senderLabel(msg('[{"email":"ada@example.com"}]'))).toBe('ada@example.com');
  });

  it('handles a bare string and emptiness', () => {
    expect(senderLabel(msg('grace@example.com'))).toBe('grace@example.com');
    expect(senderLabel(msg(''))).toBe('');
    expect(senderLabel(msg(undefined))).toBe('');
  });

  it('unwraps the {"raw": "Name <email>"} object form', () => {
    expect(senderLabel(msg('{"raw":"QQ邮箱管理员 <10000@qq.com>"}'))).toBe('QQ邮箱管理员');
    expect(senderLabel(msg('{"raw":"10000@qq.com"}'))).toBe('10000@qq.com');
  });
});

describe('isIncomingFolderRole', () => {
  it('treats inbox, archive, and custom (null) folders as incoming', () => {
    expect(isIncomingFolderRole('inbox')).toBe(true);
    expect(isIncomingFolderRole('archive')).toBe(true);
    expect(isIncomingFolderRole(null)).toBe(true);
    expect(isIncomingFolderRole(undefined)).toBe(true);
  });

  it('excludes outgoing and system folders', () => {
    for (const role of ['sent', 'drafts', 'trash', 'spam', 'junk', 'outbox']) {
      expect(isIncomingFolderRole(role)).toBe(false);
    }
  });
});

describe('messageIdentity', () => {
  it('prefers the RFC 5322 Message-ID, falling back to the row id', () => {
    const withHeader = { ...msg('x@example.com'), messageIdHeader: '<m1@example.com>' };
    expect(messageIdentity(withHeader)).toBe('<m1@example.com>');
    expect(messageIdentity(msg('x@example.com'))).toBe('m1');
  });
});
