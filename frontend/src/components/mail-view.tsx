/**
 * Right pane — reading pane with column-header icon toolbar.
 */

import { useEffect, useState, type ReactNode } from 'react';
import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { t } from '../i18n';
import { getInitials, formatMailDate } from '../lib/utils';
import type { MailMessage } from '../types';

function IconBtn({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="mail-icon-btn"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </button>
  );
}

function ArchiveIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
      <path
        d="M21 8v13H3V8M1 3h22v5H1zM10 12h4"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function TrashIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
      <path
        d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function ReplyIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
      <polyline
        points="9 17 4 12 9 7"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M20 18v-2a4 4 0 0 0-4-4H4"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function ForwardIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
      <polyline
        points="15 17 20 12 15 7"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M4 18v-2a4 4 0 0 1 4-4h12"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function quoteBody(message: MailMessage) {
  const from = message.from.name
    ? `${message.from.name} <${message.from.email}>`
    : message.from.email;
  const original = message.bodyText ?? message.snippet;
  return `\n\nOn ${new Date(message.date).toLocaleString()}, ${from} wrote:\n> ${original
    .split('\n')
    .join('\n> ')}`;
}

function parseAddr(json?: string | null): { name?: string; email: string } {
  if (!json) return { email: 'unknown' };
  try {
    const parsed = JSON.parse(json) as { raw?: string; email?: string; name?: string };
    if (parsed.email) return { name: parsed.name, email: parsed.email };
    if (parsed.raw) {
      const match = parsed.raw.match(/^(.+?)\s*<(.+?)>$/);
      if (match) return { name: match[1].trim(), email: match[2].trim() };
      return { email: parsed.raw };
    }
  } catch {
    /* fall through */
  }
  return { email: 'unknown' };
}

function parseAddrs(json?: string | null): Array<{ name?: string; email: string }> {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json) as unknown;
    if (Array.isArray(parsed)) {
      return parsed.map((item) => {
        if (typeof item === 'string') {
          const match = item.match(/^(.+?)\s*<(.+?)>$/);
          if (match) return { name: match[1].trim(), email: match[2].trim() };
          return { email: item };
        }
        if (item && typeof item === 'object' && 'email' in item) {
          const o = item as { email: string; name?: string };
          return { name: o.name, email: o.email };
        }
        return { email: 'unknown' };
      });
    }
  } catch {
    /* fall through */
  }
  return [];
}

export function MailView() {
  const locale = useUIStore((s) => s.locale);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const openCompose = useUIStore((s) => s.openCompose);
  const token = useAuthStore((s) => s.token);
  const cached = useMailStore((s) =>
    selectedMessageId ? s.messages[selectedMessageId] : undefined,
  );
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const removeMessage = useMailStore((s) => s.removeMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);

  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<
    Array<{
      id: string;
      filename?: string;
      contentType?: string;
      sizeBytes?: number;
      isInline: boolean;
    }>
  >([]);

  useEffect(() => {
    if (!selectedMessageId || !token) return;
    let cancelled = false;

    const load = async () => {
      setLoadError(null);
      try {
        const res = await fetch(`/api/messages/${selectedMessageId}`, {
          headers: { Authorization: `Bearer ${token}` },
        });
        if (!res.ok) throw new Error('Failed to load message');
        const msg = await res.json();
        if (cancelled) return;

        upsertMessage({
          id: msg.id,
          accountId: msg.accountId,
          folderId: msg.folderId,
          subject: msg.subject ?? '(no subject)',
          from: parseAddr(msg.fromAddress),
          to: parseAddrs(msg.toAddresses),
          cc: parseAddrs(msg.ccAddresses),
          date: msg.date ?? new Date().toISOString(),
          snippet: msg.snippet ?? '',
          bodyText: msg.bodyText,
          bodyHtml: msg.bodyHtml,
          isRead: msg.isRead,
          isStarred: msg.isStarred,
          isDraft: false,
          hasAttachments: msg.hasAttachments,
        });

        if (!msg.isRead) {
          const patch = await fetch(`/api/messages/${selectedMessageId}`, {
            method: 'PATCH',
            headers: {
              Authorization: `Bearer ${token}`,
              'Content-Type': 'application/json',
            },
            body: JSON.stringify({ isRead: true }),
          });
          if (patch.ok) markMessageRead(selectedMessageId);
        }

        if (msg.hasAttachments) {
          const attRes = await fetch(`/api/messages/${selectedMessageId}/attachments`, {
            headers: { Authorization: `Bearer ${token}` },
          });
          if (attRes.ok && !cancelled) {
            const atts = (await attRes.json()) as Array<{
              id: string;
              filename?: string;
              contentType?: string;
              sizeBytes?: number;
              isInline: boolean;
            }>;
            setAttachments(atts);
          }
        } else if (!cancelled) {
          setAttachments([]);
        }
      } catch (err: unknown) {
        if (!cancelled) {
          setLoadError(err instanceof Error ? err.message : 'Failed to load message');
        }
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
  }, [selectedMessageId, token, upsertMessage, markMessageRead]);

  const message = cached;

  if (!selectedMessageId) {
    return (
      <div className="mail-view-empty">
        <p>{t(locale, 'mail.selectMessage')}</p>
      </div>
    );
  }

  if (!message) {
    return (
      <div className="mail-view-empty">
        <p>{loadError ?? t(locale, 'common.loading')}</p>
      </div>
    );
  }

  const handleReply = () => {
    openCompose({
      mode: 'reply',
      to: message.from.email,
      subject: message.subject.startsWith('Re:') ? message.subject : `Re: ${message.subject}`,
      body: quoteBody(message),
    });
  };

  const handleForward = () => {
    openCompose({
      mode: 'forward',
      to: '',
      subject: message.subject.startsWith('Fwd:') ? message.subject : `Fwd: ${message.subject}`,
      body: quoteBody(message),
    });
  };

  const handleAction = async (action: 'trash' | 'archive') => {
    if (!token || busy) return;
    setBusy(true);
    try {
      const res = await fetch(`/api/messages/${message.id}/${action}`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error(`${action} failed`);
      removeMessage(message.id);
      setSelectedMessage(null);
    } catch {
      /* keep selection; user can retry */
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mail-view">
      <div className="mail-view-toolbar">
        <div className="mail-view-toolbar-group">
          <IconBtn
            label={t(locale, 'mail.archive')}
            onClick={() => void handleAction('archive')}
            disabled={busy}
          >
            <ArchiveIcon />
          </IconBtn>
          <IconBtn
            label={t(locale, 'mail.delete')}
            onClick={() => void handleAction('trash')}
            disabled={busy}
          >
            <TrashIcon />
          </IconBtn>
        </div>
        <div className="mail-view-toolbar-group">
          <IconBtn label={t(locale, 'mail.reply')} onClick={handleReply}>
            <ReplyIcon />
          </IconBtn>
          <IconBtn label={t(locale, 'mail.forward')} onClick={handleForward}>
            <ForwardIcon />
          </IconBtn>
        </div>
      </div>

      <div className="mail-view-content">
        {loadError && <p className="mail-view-error">{loadError}</p>}
        <div className="mail-view-person">
          <div className="mail-view-avatar">
            {getInitials(message.from.name ?? message.from.email)}
          </div>
          <div className="mail-view-person-meta">
            <div className="mail-view-person-name">
              {message.from.name ?? message.from.email}
            </div>
            <div className="mail-view-person-email">
              {message.from.email}
              {message.to.length > 0 &&
                ` · ${t(locale, 'mail.to')} ${message.to.map((a) => a.name ?? a.email).join(', ')}`}
            </div>
          </div>
          <time className="mail-view-person-date">{formatMailDate(message.date)}</time>
        </div>

        <h2 className="mail-view-subject">{message.subject}</h2>

        <div className="mail-view-body">
          {message.bodyHtml ? (
            // eslint-disable-next-line react/no-danger
            <div dangerouslySetInnerHTML={{ __html: message.bodyHtml }} />
          ) : (
            <pre className="mail-view-text">{message.bodyText ?? message.snippet}</pre>
          )}
        </div>

        {message.hasAttachments && (
          <div className="mail-view-attachments">
            <h3>{t(locale, 'mail.attachments')}</h3>
            <ul className="mail-attachment-list">
              {attachments.map((att) => (
                <li key={att.id}>
                  <a
                    href={`/api/attachments/${att.id}`}
                    onClick={(e) => {
                      e.preventDefault();
                      if (!token) return;
                      void fetch(`/api/attachments/${att.id}`, {
                        headers: { Authorization: `Bearer ${token}` },
                      })
                        .then((r) => r.blob())
                        .then((blob) => {
                          const url = URL.createObjectURL(blob);
                          const a = document.createElement('a');
                          a.href = url;
                          a.download = att.filename ?? 'attachment';
                          a.click();
                          URL.revokeObjectURL(url);
                        });
                    }}
                  >
                    {att.filename ?? att.id}
                    {att.sizeBytes != null ? ` (${Math.round(att.sizeBytes / 1024)} KB)` : ''}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
