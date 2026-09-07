/**
 * Notification helper unit tests: prefs persistence robustness and the
 * sender-label extraction from the stored `fromAddress` forms.
 */

import { afterEach, describe, expect, it } from 'vitest';

import {
  isFolderMuted,
  isIncomingFolderRole,
  messageIdentity,
  readNotificationPrefs,
  senderLabel,
  setFolderMuted,
  writeNotificationPrefs,
  type NotificationPrefs,
} from './notifications';
import type { ApiMessage } from './mail-api';

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
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [] });
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: true,
      mutedFolderIds: [],
    });
  });

  it('defaults when unset or corrupted', () => {
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
    });
    localStorage.setItem('lyra.notifications', '{not json');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
    });
    localStorage.setItem('lyra.notifications', '{"enabled":"yes"}');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: false,
      mutedFolderIds: [],
    });
  });

  it('reads legacy blobs without mutedFolderIds and filters junk entries', () => {
    localStorage.setItem('lyra.notifications', '{"enabled":true}');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({
      enabled: true,
      mutedFolderIds: [],
    });
    localStorage.setItem('lyra.notifications', '{"enabled":true,"mutedFolderIds":["f1",7,"f2"]}');
    expect(readNotificationPrefs().mutedFolderIds).toEqual(['f1', 'f2']);
  });

  it('mutes and unmutes folders', () => {
    writeNotificationPrefs({ enabled: true, mutedFolderIds: [] });
    setFolderMuted('f1', true);
    expect(isFolderMuted('f1')).toBe(true);
    expect(isFolderMuted('f2')).toBe(false);
    setFolderMuted('f1', false);
    expect(isFolderMuted('f1')).toBe(false);
    expect(readNotificationPrefs().enabled).toBe(true);
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
