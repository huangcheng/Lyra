import { describe, expect, it } from 'vitest';

import {
  baseSubject,
  conversationKeyOf,
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
  it('groups original and reply, latest first', () => {
    const original = msg('m1', 'Ping An request', '2026-08-25T10:00:00Z');
    const reply = msg('m2', 'Re: Ping An request', '2026-08-26T10:00:00Z', {
      isRead: false,
      isReplied: false,
    });
    const other = msg('m3', 'Unrelated', '2026-08-26T12:00:00Z');
    const convos = groupIntoConversations([reply, original, other]);
    expect(convos).toHaveLength(2);
    expect(convos[0].latest.id).toBe('m3');
    expect(convos[1].messages.map((m) => m.id)).toEqual(['m1', 'm2']);
    expect(convos[1].unreadCount).toBe(1);
  });

  it('never groups across accounts', () => {
    const a = msg('m1', 'Same subject', '2026-08-25T10:00:00Z');
    const b = { ...msg('m2', 'Re: Same subject', '2026-08-26T10:00:00Z'), accountId: 'a2' };
    expect(conversationKeyOf(a)).not.toBe(conversationKeyOf(b));
    expect(groupIntoConversations([a, b])).toHaveLength(2);
  });

  it('keeps empty subjects as singletons', () => {
    const a = msg('m1', '', '2026-08-25T10:00:00Z');
    const b = msg('m2', 'Re: ', '2026-08-26T10:00:00Z');
    expect(groupIntoConversations([a, b])).toHaveLength(2);
  });

  it('conversationMembers returns ascending order including self', () => {
    const original = msg('m1', 'Hi', '2026-08-25T10:00:00Z');
    const reply = msg('m2', 'Re: Hi', '2026-08-26T10:00:00Z');
    const all = { m1: original, m2: reply, m3: msg('m3', 'Nope', '2026-08-27T00:00:00Z') };
    expect(conversationMembers(reply, all).map((m) => m.id)).toEqual(['m1', 'm2']);
  });
});
