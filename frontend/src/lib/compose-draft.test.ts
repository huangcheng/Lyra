import { describe, expect, it } from 'vitest';

import { buildForwardDraft, buildReplyDraft, quoteBody } from '@/lib/compose-draft';
import type { MailAccount, MailMessage } from '@/types';

const accounts: MailAccount[] = [
  {
    id: 'acc1',
    displayName: 'Work',
    emailAddress: 'me@work.example',
    protocol: 'imap',
    isActive: true,
    signature: 'Cheers,\nMe',
    syncEnabled: true,
  },
];

function msg(over: Partial<MailMessage> = {}): MailMessage {
  return {
    id: 'm1',
    accountId: 'acc1',
    folderId: 'f1',
    subject: 'Hello',
    from: { name: 'Alice', email: 'alice@example.com' },
    to: [{ email: 'me@work.example' }],
    date: '2026-09-01T10:00:00Z',
    snippet: 'hi',
    bodyText: 'plain body',
    isRead: true,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
    ...over,
  };
}

describe('quoteBody', () => {
  it('quotes bodyText with > prefixes', () => {
    const out = quoteBody(msg({ bodyText: 'a\nb' }));
    expect(out).toContain('> a\n> b');
    expect(out).toContain('alice@example.com wrote:');
  });

  it('falls back to the snippet when no bodyText', () => {
    const out = quoteBody(msg({ bodyText: undefined, snippet: 'snip' }));
    expect(out).toContain('> snip');
  });
});

describe('buildReplyDraft', () => {
  it('replies to the sender and prefixes Re:', () => {
    const d = buildReplyDraft(msg(), false, accounts);
    expect(d.mode).toBe('reply');
    expect(d.accountId).toBe('acc1');
    expect(d.to).toBe('alice@example.com');
    expect(d.subject).toBe('Re: Hello');
    expect(d.initialHtml).toContain('plain body');
    expect(d.initialHtml).toContain('Cheers'); // signature above the quote
  });

  it('does not double-prefix Re:', () => {
    expect(buildReplyDraft(msg({ subject: 'Re: Hello' }), false, accounts).subject).toBe(
      'Re: Hello',
    );
  });

  it('reply-all includes original to recipients', () => {
    const d = buildReplyDraft(msg(), true, accounts);
    expect(d.to).toBe('alice@example.com, me@work.example');
  });

  it('carries inline attachments as inlineSources', () => {
    const d = buildReplyDraft(
      msg({
        attachments: [
          { id: 'a1', filename: 'x.pdf', isInline: false },
          {
            id: 'a2',
            filename: 'logo.png',
            contentType: 'image/png',
            isInline: true,
            contentId: 'cid-1@x',
          },
          { id: 'a3', filename: 'no-cid.png', isInline: true },
        ],
      }),
      false,
      accounts,
    );
    expect(d.inlineSources).toEqual([
      { id: 'a2', filename: 'logo.png', contentType: 'image/png', contentId: 'cid-1@x' },
    ]);
  });

  it('omits inlineSources when the message has no inline attachments', () => {
    expect(buildReplyDraft(msg(), false, accounts).inlineSources).toBeUndefined();
  });
});

describe('buildForwardDraft', () => {
  it('prefixes Fwd: and carries non-inline attachments', () => {
    const d = buildForwardDraft(
      msg({
        attachments: [
          { id: 'a1', filename: 'x.pdf', isInline: false },
          { id: 'a2', filename: 'logo.png', isInline: true },
        ],
      }),
      accounts,
    );
    expect(d.mode).toBe('forward');
    expect(d.accountId).toBe('acc1');
    expect(d.subject).toBe('Fwd: Hello');
    expect(d.forwardAttachments).toEqual([{ id: 'a1', filename: 'x.pdf', contentType: undefined }]);
  });

  it('omits forwardAttachments when there are none to carry', () => {
    expect(buildForwardDraft(msg(), accounts).forwardAttachments).toBeUndefined();
  });

  it('carries inline attachments as inlineSources and omits them when absent', () => {
    const withInline = buildForwardDraft(
      msg({
        attachments: [
          {
            id: 'a2',
            filename: 'logo.png',
            contentType: 'image/png',
            isInline: true,
            contentId: 'cid-1@x',
          },
        ],
      }),
      accounts,
    );
    expect(withInline.inlineSources).toEqual([
      { id: 'a2', filename: 'logo.png', contentType: 'image/png', contentId: 'cid-1@x' },
    ]);
    expect(buildForwardDraft(msg(), accounts).inlineSources).toBeUndefined();
  });
});
