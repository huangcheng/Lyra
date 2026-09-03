import { describe, expect, it } from 'vitest';

import {
  baseSubject,
  conversationMembers,
  groupIntoConversations,
  normalizeSubject,
} from '@/lib/conversation';
import type { MailMessage } from '@/types';

function msg(id: string, subject: string, date: string, extra: Partial<MailMessage> = {}) {
  return {
    id,
    accountId: 'a1',
    folderId: 'f1',
    subject,
    from: { email: 'a@example.com' },
    to: [],
    date,
    snippet: '',
    isRead: true,
    isStarred: false,
    hasAttachments: false,
    ...extra,
  } as MailMessage;
}

describe('baseSubject', () => {
  it('strips reply/forward prefixes in any language and case', () => {
    expect(baseSubject('Re: Hello')).toBe('Hello');
    expect(baseSubject('RE: Fwd: 答复： Hello')).toBe('Hello');
    expect(baseSubject('回复：转发: hi')).toBe('hi');
    expect(baseSubject('No prefix')).toBe('No prefix');
  });

  it('collapses whitespace', () => {
    expect(baseSubject('Re:  hello   world ')).toBe('hello world');
  });
});

describe('normalizeSubject', () => {
  it('lowercases for matching', () => {
    expect(normalizeSubject('Re: Hello World')).toBe('hello world');
  });
});

describe('grouping', () => {
  it('threads by RFC 5322 References, latest first', () => {
    const original = msg('m1', 'Ping An request', '2026-08-25T10:00:00Z', {
      messageIdHeader: '<a@x>',
    });
    const reply = msg('m2', 'Re: Ping An request', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<b@x>',
      inReplyTo: '<a@x>',
      referencesHeaders: '<a@x>',
      isRead: false,
    });
    const other = msg('m3', 'Unrelated', '2026-08-26T12:00:00Z', {
      messageIdHeader: '<c@x>',
    });
    const convos = groupIntoConversations([reply, original, other]);
    expect(convos).toHaveLength(2);
    expect(convos[0].latest.id).toBe('m3');
    expect(convos[1].messages.map((m) => m.id)).toEqual(['m1', 'm2']);
    expect(convos[1].unreadCount).toBe(1);
  });

  it('never threads across accounts even with matching references', () => {
    const a = msg('m1', 'Same subject', '2026-08-25T10:00:00Z', {
      messageIdHeader: '<a@x>',
    });
    const b = {
      ...msg('m2', 'Re: Same subject', '2026-08-26T10:00:00Z', {
        messageIdHeader: '<b@x>',
        inReplyTo: '<a@x>',
      }),
      accountId: 'a2',
    };
    expect(groupIntoConversations([a, b])).toHaveLength(2);
  });

  it('RFC 5322 regression: same-subject automail (verification codes) never groups', () => {
    const code1 = msg('m1', 'Your verification code', '2026-08-25T10:00:00Z', {
      messageIdHeader: '<c1@svc>',
    });
    const code2 = msg('m2', 'Your verification code', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<c2@svc>',
    });
    const code3 = msg('m3', 'Your verification code', '2026-08-27T10:00:00Z', {
      messageIdHeader: '<c3@svc>',
    });
    expect(groupIntoConversations([code1, code2, code3])).toHaveLength(3);
  });

  it('explicit Re: prefix threads without References (fallback for broken clients)', () => {
    const original = msg('m1', 'Ticket 123', '2026-08-25T10:00:00Z', {
      messageIdHeader: '<a@x>',
    });
    const reply = msg('m2', 'Re: Ticket 123', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<b@x>',
    });
    const convos = groupIntoConversations([original, reply]);
    expect(convos).toHaveLength(1);
    expect(convos[0].messages.map((m) => m.id)).toEqual(['m1', 'm2']);
  });

  it('forward prefix does NOT thread (forwards start new threads)', () => {
    const original = msg('m1', 'Ticket 123', '2026-08-25T10:00:00Z', {
      messageIdHeader: '<a@x>',
    });
    const fwd = msg('m2', 'Fwd: Ticket 123', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<b@x>',
    });
    expect(groupIntoConversations([original, fwd])).toHaveLength(2);
  });

  it('threads transitively through References chains', () => {
    const m1 = msg('m1', 'Root', '2026-08-25T10:00:00Z', { messageIdHeader: '<a@x>' });
    const m2 = msg('m2', 'Re: Root', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<b@x>',
      inReplyTo: '<a@x>',
      referencesHeaders: '<a@x>',
    });
    // Replies to m2 without naming m1's id directly — linked via m2.
    const m3 = msg('m3', 'Re: Root', '2026-08-27T10:00:00Z', {
      messageIdHeader: '<c@x>',
      inReplyTo: '<b@x>',
      referencesHeaders: '<a@x> <b@x>',
    });
    const convos = groupIntoConversations([m3, m1, m2]);
    expect(convos).toHaveLength(1);
    expect(convos[0].messages.map((m) => m.id)).toEqual(['m1', 'm2', 'm3']);
  });

  it('keeps empty subjects as singletons', () => {
    const a = msg('m1', '', '2026-08-25T10:00:00Z');
    const b = msg('m2', 'Re: ', '2026-08-26T10:00:00Z');
    expect(groupIntoConversations([a, b])).toHaveLength(2);
  });

  it('conversationMembers returns ascending order including self', () => {
    const original = msg('m1', 'Hi', '2026-08-25T10:00:00Z', { messageIdHeader: '<a@x>' });
    const reply = msg('m2', 'Re: Hi', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<b@x>',
      inReplyTo: '<a@x>',
    });
    const all = { m1: original, m2: reply, m3: msg('m3', 'Nope', '2026-08-27T00:00:00Z') };
    expect(conversationMembers(reply, all).map((m) => m.id)).toEqual(['m1', 'm2']);
  });

  it('collapses cross-folder copies by Message-ID, keeping the viewed copy', () => {
    const inboxCopy = msg('m1', 'Hi', '2026-08-25T10:00:00Z', {
      folderId: 'inbox',
      messageIdHeader: '<abc@x>',
    });
    const archiveCopy = msg('m1b', 'Hi', '2026-08-25T10:00:00Z', {
      folderId: 'archive',
      messageIdHeader: '<abc@x>',
      bodyText: 'full body',
    });
    const reply = msg('m2', 'Re: Hi', '2026-08-26T10:00:00Z', {
      messageIdHeader: '<def@x>',
      inReplyTo: '<abc@x>',
      referencesHeaders: '<abc@x>',
    });
    const all = { m1: inboxCopy, m1b: archiveCopy, m2: reply };
    // Selecting the inbox copy keeps it even though the archive copy has a body.
    expect(conversationMembers(inboxCopy, all).map((m) => m.id)).toEqual(['m1', 'm2']);
    // From the archive folder the archive copy wins instead.
    expect(conversationMembers(archiveCopy, all).map((m) => m.id)).toEqual(['m1b', 'm2']);
  });

  it('header-less same-subject copies are separate threads (subject never groups)', () => {
    const a = msg('m1', 'Hi', '2026-08-25T10:00:00Z', { folderId: 'inbox' });
    const b = msg('m2', 'Hi', '2026-08-25T10:00:00Z', { folderId: 'archive' });
    expect(conversationMembers(a, { m1: a, m2: b })).toHaveLength(1);
  });
});
