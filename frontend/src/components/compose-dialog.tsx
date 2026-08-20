/**
 * Compose dialog for writing new emails.
 *
 * Full-screen modal with To, Subject, and body fields.
 * Sends via POST /api/messages/send using the selected account's SMTP.
 */

import { useState } from 'react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';
import { useMailStore } from '../stores/mail';
import { useAuthStore } from '../stores/auth';

interface ComposeForm {
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
}

export function ComposeDialog() {
  const locale = useUIStore((s) => s.locale);
  const composeOpen = useUIStore((s) => s.composeOpen);
  const setComposeOpen = useUIStore((s) => s.setComposeOpen);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const accounts = useMailStore((s) => s.accounts);
  const token = useAuthStore((s) => s.token);

  const [form, setForm] = useState<ComposeForm>({
    to: '',
    cc: '',
    bcc: '',
    subject: '',
    body: '',
  });
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  if (!composeOpen) return null;

  // Use the selected account, or the first available account
  const accountId = selectedAccountId ?? accounts[0]?.id;

  const handleClose = () => {
    setComposeOpen(false);
    setForm({ to: '', cc: '', bcc: '', subject: '', body: '' });
    setError(null);
    setSuccess(false);
  };

  const handleSend = async () => {
    if (!form.to.trim()) {
      setError(t(locale, 'mail.to') + ' is required');
      return;
    }
    if (!form.subject.trim()) {
      setError(t(locale, 'mail.subject') + ' is required');
      return;
    }
    if (!accountId) {
      setError('No account selected');
      return;
    }

    setSending(true);
    setError(null);

    try {
      const toRecipients = form.to
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean)
        .map((email) => ({ email }));

      const ccRecipients = form.cc
        ? form.cc
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean)
            .map((email) => ({ email }))
        : [];

      const bccRecipients = form.bcc
        ? form.bcc
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean)
            .map((email) => ({ email }))
        : [];

      const res = await fetch('/api/messages/send', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({
          accountId,
          to: toRecipients,
          cc: ccRecipients,
          bcc: bccRecipients,
          subject: form.subject,
          bodyText: form.body,
          bodyHtml: null,
        }),
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || t(locale, 'mail.sendError'));
      }

      setSuccess(true);
      setTimeout(() => handleClose(), 1500);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : t(locale, 'mail.sendError'));
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="compose-overlay" onClick={handleClose}>
      <div className="compose-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="compose-header">
          <h2>{t(locale, 'mail.compose')}</h2>
          <button type="button" className="compose-close" onClick={handleClose} aria-label="Close">
            ✕
          </button>
        </div>

        <div className="compose-fields">
          <div className="compose-field">
            <label htmlFor="compose-to">{t(locale, 'mail.to')}</label>
            <input
              id="compose-to"
              type="text"
              value={form.to}
              onChange={(e) => setForm((f) => ({ ...f, to: e.target.value }))}
              placeholder={t(locale, 'mail.toPlaceholder')}
              autoFocus
            />
          </div>

          <div className="compose-field">
            <label htmlFor="compose-cc">{t(locale, 'mail.cc')}</label>
            <input
              id="compose-cc"
              type="text"
              value={form.cc}
              onChange={(e) => setForm((f) => ({ ...f, cc: e.target.value }))}
              placeholder={t(locale, 'mail.toPlaceholder')}
            />
          </div>

          <div className="compose-field">
            <label htmlFor="compose-bcc">{t(locale, 'mail.bcc')}</label>
            <input
              id="compose-bcc"
              type="text"
              value={form.bcc}
              onChange={(e) => setForm((f) => ({ ...f, bcc: e.target.value }))}
              placeholder={t(locale, 'mail.toPlaceholder')}
            />
          </div>

          <div className="compose-field">
            <label htmlFor="compose-subject">{t(locale, 'mail.subject')}</label>
            <input
              id="compose-subject"
              type="text"
              value={form.subject}
              onChange={(e) => setForm((f) => ({ ...f, subject: e.target.value }))}
              placeholder={t(locale, 'mail.subjectPlaceholder')}
            />
          </div>
        </div>

        <div className="compose-body">
          <textarea
            value={form.body}
            onChange={(e) => setForm((f) => ({ ...f, body: e.target.value }))}
            placeholder={t(locale, 'mail.bodyPlaceholder')}
          />
        </div>

        {error && <div className="compose-error">{error}</div>}
        {success && <div className="compose-success">{t(locale, 'mail.sendSuccess')}</div>}

        <div className="compose-actions">
          <button
            type="button"
            className="compose-send-btn"
            onClick={handleSend}
            disabled={sending}
          >
            {sending ? t(locale, 'mail.sending') : t(locale, 'mail.send')}
          </button>
          <button
            type="button"
            className="compose-discard-btn"
            onClick={handleClose}
            disabled={sending}
          >
            {t(locale, 'mail.discard')}
          </button>
        </div>
      </div>
    </div>
  );
}
