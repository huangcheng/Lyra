/**
 * Conversation grouping (client-side).
 *
 * The backend `thread` table is not populated yet, so conversations are
 * derived from messages already in the store: same account + same
 * subject with reply/forward prefixes stripped. This matches how
 * Fastmail/Outlook present a thread — one row in the list, stacked
 * message cards in the reader.
 */

import type { MailMessage } from '@/types';

const REPLY_PREFIX = /^(re|fw|fwd|答复|回复|转发)\s*[:：]\s*/i;

/** Subject without any leading Re:/Fwd:/答复: chain, whitespace-normalized. */
export function baseSubject(subject: string): string {
  let s = (subject || '').replace(/\s+/g, ' ').trim();
  while (REPLY_PREFIX.test(s)) s = s.replace(REPLY_PREFIX, '').trim();
  return s;
}

/** Case-insensitive key form of {@link baseSubject}. */
export function normalizeSubject(subject: string): string {
  return baseSubject(subject).toLowerCase();
}

/**
 * Stable conversation key. Messages with an empty base subject never
 * group together — they each become a singleton keyed by message id.
 */
export function conversationKeyOf(message: Pick<MailMessage, 'id' | 'accountId' | 'subject'>) {
  const subject = normalizeSubject(message.subject);
  if (!subject) return `${message.accountId}::#${message.id}`;
  return `${message.accountId}::${subject}`;
}

export interface Conversation {
  key: string;
  /** Members ascending by date (oldest first). */
  messages: MailMessage[];
  latest: MailMessage;
  unreadCount: number;
  anyStarred: boolean;
  anyReplied: boolean;
}

function byDateAsc(a: MailMessage, b: MailMessage) {
  return new Date(a.date).getTime() - new Date(b.date).getTime();
}

/** Group a message list into conversations, newest conversation first. */
export function groupIntoConversations(messages: MailMessage[]): Conversation[] {
  const buckets = new Map<string, MailMessage[]>();
  for (const message of messages) {
    const key = conversationKeyOf(message);
    const bucket = buckets.get(key);
    if (bucket) bucket.push(message);
    else buckets.set(key, [message]);
  }
  const conversations: Conversation[] = [];
  for (const [key, members] of buckets) {
    members.sort(byDateAsc);
    conversations.push({
      key,
      messages: members,
      latest: members[members.length - 1],
      unreadCount: members.filter((m) => !m.isRead).length,
      anyStarred: members.some((m) => m.isStarred),
      anyReplied: members.some((m) => m.isReplied),
    });
  }
  conversations.sort(
    (a, b) => new Date(b.latest.date).getTime() - new Date(a.latest.date).getTime(),
  );
  return conversations;
}

/**
 * All store messages belonging to the same conversation as `message`,
 * ascending by date. Always contains `message` itself.
 */
export function conversationMembers(
  message: MailMessage,
  all: Record<string, MailMessage>,
): MailMessage[] {
  const key = conversationKeyOf(message);
  return Object.values(all)
    .filter((m) => conversationKeyOf(m) === key)
    .sort(byDateAsc);
}
