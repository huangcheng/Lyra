/**
 * Compose-draft builders shared by the reading pane and the list context
 * menu. Pure functions: given a message (+ accounts for the signature),
 * produce the Partial<ComposeDraft> for `useUIStore.openCompose`.
 */

import { forwardHtml, quotedReplyHtml } from '@/lib/compose-html';
import { sanitizeEmailHtml } from '@/lib/sanitize-email-html';
import type { ComposeDraft } from '@/stores/ui';
import type { MailAccount, MailMessage } from '@/types';

/** Plain-text fallback quote used as ComposeDraft.body. */
export function quoteBody(
  message: Pick<MailMessage, 'from' | 'date' | 'snippet'> & { bodyText?: string },
): string {
  const quoted = (message.bodyText ?? message.snippet)
    .split('\n')
    .map((line) => `> ${line}`)
    .join('\n');
  return `\n\nOn ${message.date}, ${message.from.email} wrote:\n${quoted}`;
}

function signatureOf(accounts: MailAccount[], accountId: string): string | undefined {
  return accounts.find((a) => a.id === accountId)?.signature ?? undefined;
}

function quoteSource(m: MailMessage) {
  return {
    fromName: m.from.name ?? '',
    fromEmail: m.from.email,
    date: m.date,
    bodyHtml: m.bodyHtml ? sanitizeEmailHtml(m.bodyHtml) : undefined,
    bodyText: m.bodyText,
  };
}

/** Inline (cid:) parts of the source message — the dialog resolves these to
 * object URLs so quoted inline images render and re-attach on send. */
export function inlineSourcesOf(m: MailMessage): ComposeDraft['inlineSources'] {
  const sources = (m.attachments ?? [])
    .filter((a) => a.isInline && a.contentId)
    .map((a) => ({
      id: a.id,
      filename: a.filename,
      contentType: a.contentType,
      contentId: a.contentId ?? undefined,
    }));
  return sources.length > 0 ? sources : undefined;
}

/** Reply draft; `all` adds the original To recipients. */
export function buildReplyDraft(
  m: MailMessage,
  all: boolean,
  accounts: MailAccount[],
): Partial<ComposeDraft> {
  const to = all
    ? [m.from.email, ...m.to.map((a) => a.email)].filter(Boolean).join(', ')
    : m.from.email;
  return {
    mode: 'reply',
    accountId: m.accountId,
    to,
    subject: m.subject.startsWith('Re:') ? m.subject : `Re: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: quotedReplyHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
    inlineSources: inlineSourcesOf(m),
  };
}

/** Forward draft; carries the original's non-inline attachment metadata. */
export function buildForwardDraft(m: MailMessage, accounts: MailAccount[]): Partial<ComposeDraft> {
  const forwardAttachments = (m.attachments ?? [])
    .filter((a) => !a.isInline)
    .map((a) => ({ id: a.id, filename: a.filename, contentType: a.contentType }));
  return {
    mode: 'forward',
    accountId: m.accountId,
    to: '',
    subject: m.subject.startsWith('Fwd:') ? m.subject : `Fwd: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: forwardHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
    forwardAttachments: forwardAttachments.length > 0 ? forwardAttachments : undefined,
    inlineSources: inlineSourcesOf(m),
  };
}
