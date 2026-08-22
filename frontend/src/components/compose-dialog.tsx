/**
 * Compose dialog for writing new emails.
 */

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { t } from '@/i18n';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

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
  const composeDraft = useUIStore((s) => s.composeDraft);
  const setComposeOpen = useUIStore((s) => s.setComposeOpen);
  const clearComposeDraft = useUIStore((s) => s.clearComposeDraft);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const accounts = useMailStore((s) => s.accounts);
  const token = useAuthStore((s) => s.token);

  const [fromAccountId, setFromAccountId] = useState('');
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

  useEffect(() => {
    if (!composeOpen) return;
    setForm({
      to: composeDraft?.to ?? '',
      cc: '',
      bcc: '',
      subject: composeDraft?.subject ?? '',
      body: composeDraft?.body ?? '',
    });
    setFromAccountId(
      selectedAccountId === ALL_ACCOUNTS ? (accounts[0]?.id ?? '') : selectedAccountId,
    );
    setError(null);
    setSuccess(false);
  }, [composeOpen, composeDraft, selectedAccountId, accounts]);

  const titleKey =
    composeDraft?.mode === 'reply'
      ? 'mail.reply'
      : composeDraft?.mode === 'forward'
        ? 'mail.forward'
        : 'mail.compose';

  const handleClose = () => {
    setComposeOpen(false);
    clearComposeDraft();
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
    if (!fromAccountId) {
      setError(t(locale, 'settings.accounts.empty'));
      return;
    }

    setSending(true);
    setError(null);

    try {
      const split = (value: string) =>
        value
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean)
          .map((email) => ({ email }));

      const res = await fetch('/api/messages/send', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({
          accountId: fromAccountId,
          to: split(form.to),
          cc: split(form.cc),
          bcc: split(form.bcc),
          subject: form.subject,
          bodyText: form.body,
          bodyHtml: null,
        }),
      });

      if (!res.ok) {
        const data = (await res.json().catch(() => ({}))) as { error?: string };
        throw new Error(data.error || t(locale, 'mail.sendError'));
      }

      setSuccess(true);
      window.setTimeout(() => handleClose(), 1500);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : t(locale, 'mail.sendError'));
    } finally {
      setSending(false);
    }
  };

  return (
    <Dialog open={composeOpen} onOpenChange={(open) => !open && handleClose()}>
      <DialogContent className="sm:max-w-xl" showCloseButton>
        <DialogHeader>
          <DialogTitle>{t(locale, titleKey)}</DialogTitle>
        </DialogHeader>
        <FieldGroup>
          {accounts.length > 1 ? (
            <Field>
              <FieldLabel htmlFor="compose-from">{t(locale, 'mail.from')}</FieldLabel>
              <select
                id="compose-from"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                value={fromAccountId}
                onChange={(e) => setFromAccountId(e.target.value)}
              >
                {accounts.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.displayName || a.emailAddress}
                  </option>
                ))}
              </select>
            </Field>
          ) : null}
          <Field>
            <FieldLabel htmlFor="compose-to">{t(locale, 'mail.to')}</FieldLabel>
            <Input
              id="compose-to"
              value={form.to}
              onChange={(e) => setForm((f) => ({ ...f, to: e.target.value }))}
              placeholder={t(locale, 'mail.toPlaceholder')}
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="compose-subject">{t(locale, 'mail.subject')}</FieldLabel>
            <Input
              id="compose-subject"
              value={form.subject}
              onChange={(e) => setForm((f) => ({ ...f, subject: e.target.value }))}
              placeholder={t(locale, 'mail.subjectPlaceholder')}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="compose-body">{t(locale, 'mail.bodyPlaceholder')}</FieldLabel>
            <Textarea
              id="compose-body"
              className="min-h-40"
              value={form.body}
              onChange={(e) => setForm((f) => ({ ...f, body: e.target.value }))}
              placeholder={t(locale, 'mail.bodyPlaceholder')}
            />
          </Field>
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
          {success ? (
            <p className="text-sm text-muted-foreground">{t(locale, 'mail.sendSuccess')}</p>
          ) : null}
        </FieldGroup>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={handleClose} disabled={sending}>
            {t(locale, 'mail.discard')}
          </Button>
          <Button type="button" onClick={() => void handleSend()} disabled={sending}>
            {sending ? t(locale, 'mail.sending') : t(locale, 'mail.send')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
