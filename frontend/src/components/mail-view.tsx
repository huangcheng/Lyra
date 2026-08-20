/**
 * Right pane — reading pane for the selected message.
 *
 * Modeled after the shadcn mail example reading pane.
 */

import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { t } from '../i18n';

export function MailView() {
  const locale = useUIStore((s) => s.locale);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const message = useMailStore((s) =>
    selectedMessageId ? s.messages[selectedMessageId] : undefined,
  );

  if (!message) {
    return (
      <div className="mail-view-empty">
        <p>{t(locale, 'mail.selectMessage')}</p>
      </div>
    );
  }

  return (
    <div className="mail-view">
      <div className="mail-view-header">
        <h2 className="mail-view-subject">{message.subject}</h2>
        <div className="mail-view-actions">
          <button type="button" className="mail-action-btn">
            {t(locale, 'mail.reply')}
          </button>
          <button type="button" className="mail-action-btn">
            {t(locale, 'mail.forward')}
          </button>
          <button type="button" className="mail-action-btn">
            {t(locale, 'mail.archive')}
          </button>
          <button type="button" className="mail-action-btn">
            {t(locale, 'mail.delete')}
          </button>
        </div>
      </div>

      <div className="mail-view-meta">
        <div className="mail-meta-row">
          <span className="mail-meta-label">{t(locale, 'mail.from')}</span>
          <span className="mail-meta-value">
            {message.from.name ?? message.from.email}
            {message.from.name && (
              <span className="mail-meta-email"> &lt;{message.from.email}&gt;</span>
            )}
          </span>
        </div>
        <div className="mail-meta-row">
          <span className="mail-meta-label">{t(locale, 'mail.to')}</span>
          <span className="mail-meta-value">
            {message.to.map((a) => a.name ?? a.email).join(', ')}
          </span>
        </div>
        {message.cc && message.cc.length > 0 && (
          <div className="mail-meta-row">
            <span className="mail-meta-label">{t(locale, 'mail.cc')}</span>
            <span className="mail-meta-value">
              {message.cc.map((a) => a.name ?? a.email).join(', ')}
            </span>
          </div>
        )}
        <div className="mail-meta-row">
          <span className="mail-meta-label">{t(locale, 'mail.date')}</span>
          <span className="mail-meta-value">{new Date(message.date).toLocaleString()}</span>
        </div>
      </div>

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
          {/* Attachment list would go here */}
        </div>
      )}
    </div>
  );
}
