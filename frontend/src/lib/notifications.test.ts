/**
 * Notification helper unit tests: prefs persistence robustness and the
 * sender-label extraction from the stored `fromAddress` forms.
 */

import { afterEach, describe, expect, it } from 'vitest';

import {
  readNotificationPrefs,
  senderLabel,
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
    writeNotificationPrefs({ enabled: true });
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({ enabled: true });
  });

  it('defaults when unset or corrupted', () => {
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({ enabled: false });
    localStorage.setItem('lyra.notifications', '{not json');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({ enabled: false });
    localStorage.setItem('lyra.notifications', '{"enabled":"yes"}');
    expect(readNotificationPrefs()).toEqual<NotificationPrefs>({ enabled: false });
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
