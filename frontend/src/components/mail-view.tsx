/**
 * Right pane — reading pane with column-header icon toolbar.
 */

import { useEffect, type ReactNode } from 'react';
import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { t } from '../i18n';
import { getInitials, formatMailDate } from '../lib/utils';
import type { MailMessage } from '../types';

function IconBtn({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick?: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="mail-icon-btn"
      aria-label={label}
      title={label}
      onClick={onClick}
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

export function MailView() {
  const locale = useUIStore((s) => s.locale);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const openCompose = useUIStore((s) => s.openCompose);
  const message = useMailStore((s) =>
    selectedMessageId ? s.messages[selectedMessageId] : undefined,
  );
  const removeMessage = useMailStore((s) => s.removeMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);

  useEffect(() => {
    if (message && !message.isRead) {
      markMessageRead(message.id);
    }
  }, [message, markMessageRead]);

  if (!message) {
    return (
      <div className="mail-view-empty">
        <p>{t(locale, 'mail.selectMessage')}</p>
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

  const handleRemove = () => {
    removeMessage(message.id);
    setSelectedMessage(null);
  };

  return (
    <div className="mail-view">
      <div className="mail-view-toolbar">
        <div className="mail-view-toolbar-group">
          <IconBtn label={t(locale, 'mail.archive')} onClick={handleRemove}>
            <ArchiveIcon />
          </IconBtn>
          <IconBtn label={t(locale, 'mail.delete')} onClick={handleRemove}>
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
          </div>
        )}
      </div>
    </div>
  );
}
