/**
 * Conversation threading (client-side), per RFC 5322 §3.6.4.
 *
 * Messages chain into a thread when their `In-Reply-To`/`References`
 * Message-IDs link to another message in the set (JWZ-style union-find).
 * A conservative fallback covers clients that reply without threading
 * headers: an explicit `Re:`/`回复:` prefix joins the most recent earlier
 * same-subject message in the account. Subject similarity alone NEVER
 * threads — verification codes and other same-subject automail are
 * logically unrelated messages and must stay separate.
 *
 * The backend `thread` table is not populated yet; when it is, this
 * module becomes a thin projection of server thread ids.
 */

import type { MailMessage } from '@/types';

const REPLY_PREFIX = /^(re|fw|fwd|答复|回复|转发)\s*[:：]\s*/i;
/** Strictly reply prefixes — forwards intentionally start new threads. */
const REPLY_ONLY_PREFIX = /^(re|回复)\s*[:：]\s*/i;

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

export interface Conversation {
  key: string;
  /** Members ascending by date (oldest first), cross-folder copies deduped. */
  messages: MailMessage[];
  latest: MailMessage;
  unreadCount: number;
  anyStarred: boolean;
  anyReplied: boolean;
}

function byDateAsc(a: MailMessage, b: MailMessage) {
  return new Date(a.date).getTime() - new Date(b.date).getTime();
}

/** `<id@host>` (or bare token) extraction from In-Reply-To/References. */
export function messageIdTokens(field: string | undefined): string[] {
  if (!field) return [];
  const tokens: string[] = [];
  const re = /<[^<>]+>/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(field)) !== null) tokens.push(m[0]);
  if (tokens.length === 0) {
    // Bare ids (rare, non-conforming senders): whitespace-separated words.
    return field
      .trim()
      .split(/\s+/)
      .filter((w) => w.length > 0);
  }
  return tokens;
}

function normMsgId(id: string): string {
  return id.trim().replace(/^<|>$/g, '').toLowerCase();
}

/**
 * Union-find threading over a message set. Returns root-message-id →
 * members ascending by date. Messages without links are their own root.
 */
export function linkThreads(messages: MailMessage[]): Map<string, MailMessage[]> {
  const parent = new Map<string, string>();
  const find = (id: string): string => {
    let root = id;
    while (parent.get(root) !== root) root = parent.get(root) ?? root;
    // Path compression.
    let cur = id;
    while (cur !== root) {
      const next = parent.get(cur) ?? root;
      parent.set(cur, root);
      cur = next;
    }
    return root;
  };
  const union = (a: string, b: string) => {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent.set(ra, rb);
  };

  const sorted = [...messages].sort(byDateAsc);
  for (const m of sorted) parent.set(m.id, m.id);

  // Message-ID index, scoped per account (ids are globally unique per RFC
  // 5322, but scope defensively against broken senders).
  const byMsgId = new Map<string, string>();
  for (const m of sorted) {
    const header = m.messageIdHeader?.trim();
    if (!header) continue;
    const key = `${m.accountId}|${normMsgId(header)}`;
    const existing = byMsgId.get(key);
    if (existing) {
      // Identical RFC 5322 Message-ID = the same message (cross-folder
      // copy); thread them so the reader's dedupe can pick the copy.
      union(m.id, existing);
    } else {
      byMsgId.set(key, m.id);
    }
  }

  // Latest earlier same-subject message per (account, baseSubject) for the
  // explicit-reply-prefix fallback.
  const lastBySubject = new Map<string, string>();

  for (const m of sorted) {
    let linked = false;
    const refs = [...messageIdTokens(m.inReplyTo), ...messageIdTokens(m.referencesHeaders)];
    for (const token of refs) {
      const target = byMsgId.get(`${m.accountId}|${normMsgId(token)}`);
      if (target) {
        union(m.id, target);
        linked = true;
      }
    }
    const subject = m.subject ?? '';
    const subjectKey = `${m.accountId}|${normalizeSubject(subject)}`;
    if (!linked && subject && REPLY_ONLY_PREFIX.test(subject.trim())) {
      const target = lastBySubject.get(subjectKey);
      if (target) union(m.id, target);
    }
    if (subject) lastBySubject.set(subjectKey, m.id);
  }

  const groups = new Map<string, MailMessage[]>();
  for (const m of sorted) {
    const root = find(m.id);
    const bucket = groups.get(root);
    if (bucket) bucket.push(m);
    else groups.set(root, [m]);
  }
  return groups;
}

function conversationOfGroup(root: string, members: MailMessage[]): Conversation {
  return {
    key: `th:${root}`,
    messages: members,
    latest: members[members.length - 1],
    unreadCount: members.filter((m) => !m.isRead).length,
    anyStarred: members.some((m) => m.isStarred),
    anyReplied: members.some((m) => m.isReplied),
  };
}

/** Group a message list into conversations, newest conversation first. */
export function groupIntoConversations(messages: MailMessage[]): Conversation[] {
  const groups = linkThreads(messages);
  const conversations = [...groups.entries()].map(([root, members]) =>
    conversationOfGroup(root, members),
  );
  conversations.sort(
    (a, b) => new Date(b.latest.date).getTime() - new Date(a.latest.date).getTime(),
  );
  return conversations;
}

/**
 * All store messages belonging to the same thread as `message`, ascending
 * by date. Always contains `message` itself.
 *
 * Cross-folder copies of one message (same RFC 5322 Message-ID — e.g.
 * INBOX + Archive) collapse to a single card: prefer the copy in
 * `preferFolderId` (the folder being viewed), then a copy that already
 * has a body, then the newest.
 */
export function conversationMembers(
  message: MailMessage,
  all: Record<string, MailMessage>,
): MailMessage[] {
  const groups = linkThreads(Object.values(all));
  for (const members of groups.values()) {
    if (members.some((m) => m.id === message.id)) {
      return dedupeCopies(members, message.folderId, message.id);
    }
  }
  return [message];
}

function dedupeCopies(
  members: MailMessage[],
  preferFolderId: string,
  selectedId: string,
): MailMessage[] {
  const byHeader = new Map<string, MailMessage[]>();
  const singles: MailMessage[] = [];
  for (const m of members) {
    const header = m.messageIdHeader?.trim();
    if (!header) {
      singles.push(m);
      continue;
    }
    const group = byHeader.get(header);
    if (group) group.push(m);
    else byHeader.set(header, [m]);
  }
  const kept: MailMessage[] = [...singles];
  for (const group of byHeader.values()) {
    group.sort(
      (a, b) => scoreCopy(b, preferFolderId, selectedId) - scoreCopy(a, preferFolderId, selectedId),
    );
    kept.push(group[0]);
  }
  return kept.sort(byDateAsc);
}

function scoreCopy(m: MailMessage, preferFolderId: string, selectedId: string): number {
  let score = 0;
  if (m.folderId === preferFolderId) score += 4;
  if (m.id === selectedId) score += 2;
  if (m.bodyHtml || m.bodyText) score += 1;
  return score;
}
