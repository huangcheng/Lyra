/**
 * Compose dialog for writing new emails.
 */

import { useEffect, useMemo, useState } from 'react';

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
import { api } from '@/lib/api-client';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import {
  lookupRecipientKeys,
  type RecipientKeyLookup,
  type OpengpgSendOptions,
} from '@/lib/opengpg-api';
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
  const [signMessage, setSignMessage] = useState(false);
  const [encryptMessage, setEncryptMessage] = useState(false);
  const [attachPublicKey, setAttachPublicKey] = useState(false);
  const [recipientKeys, setRecipientKeys] = useState<RecipientKeyLookup[]>([]);
  const [recipientKeyIds, setRecipientKeyIds] = useState<Record<string, string>>({});

  const recipientEmails = useMemo(() => {
    const split = (value: string) =>
      value
        .split(',')
        .map((s) => s.trim().toLowerCase())
        .filter((e) => e.includes('@'));
    return [...split(form.to), ...split(form.cc), ...split(form.bcc)];
  }, [form.to, form.cc, form.bcc]);

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
    setSignMessage(false);
    setEncryptMessage(false);
    setAttachPublicKey(false);
    setRecipientKeys([]);
    setRecipientKeyIds({});
  }, [composeOpen, composeDraft, selectedAccountId, accounts]);

  useEffect(() => {
    if (!composeOpen || !encryptMessage || recipientEmails.length === 0) {
      setRecipientKeys([]);
      return;
    }
    let cancelled = false;
    void lookupRecipientKeys(recipientEmails)
      .then((rows) => {
        if (cancelled) return;
        setRecipientKeys(rows);
        setRecipientKeyIds((prev) => {
          const next = { ...prev };
          for (const row of rows) {
            if (row.selectedKeyId && !next[row.email]) {
              next[row.email] = row.selectedKeyId;
            }
          }
          return next;
        });
      })
      .catch(() => {
        if (!cancelled) setRecipientKeys([]);
      });
    return () => {
      cancelled = true;
    };
  }, [composeOpen, encryptMessage, recipientEmails.join(',')]);

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
    setSignMessage(false);
    setEncryptMessage(false);
    setAttachPublicKey(false);
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

      const opengpg: OpengpgSendOptions | undefined =
        signMessage || encryptMessage || attachPublicKey
          ? {
              sign: signMessage,
              encrypt: encryptMessage,
              attachPublicKey,
              recipientKeyIds:
                Object.keys(recipientKeyIds).length > 0 ? recipientKeyIds : undefined,
            }
          : undefined;

      await api('/messages/send', {
        method: 'POST',
        body: JSON.stringify({
          accountId: fromAccountId,
          to: split(form.to),
          cc: split(form.cc),
          bcc: split(form.bcc),
          subject: form.subject,
          bodyText: form.body,
          bodyHtml: null,
          opengpg,
        }),
      });

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
          {accounts.length > 1 || selectedAccountId === ALL_ACCOUNTS ? (
            <Field>
              <FieldLabel htmlFor="compose-from">{t(locale, 'mail.from')}</FieldLabel>
              <select
                id="compose-from"
                className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
                value={fromAccountId}
                onChange={(e) => setFromAccountId(e.target.value)}
                required={selectedAccountId === ALL_ACCOUNTS}
              >
                {selectedAccountId === ALL_ACCOUNTS && accounts.length > 1 ? (
                  <option value="" disabled>
                    {t(locale, 'mail.fromPick')}
                  </option>
                ) : null}
                {accounts.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.emailAddress}
                    {a.displayName && a.displayName !== a.emailAddress ? ` (${a.displayName})` : ''}
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
          <Field className="gap-3 rounded-md border border-border/60 p-3">
            <p className="text-sm font-medium">{t(locale, 'mail.opengpg.composeTitle')}</p>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={signMessage}
                onChange={(e) => setSignMessage(e.target.checked)}
              />
              {t(locale, 'mail.opengpg.sign')}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={encryptMessage}
                onChange={(e) => setEncryptMessage(e.target.checked)}
              />
              {t(locale, 'mail.opengpg.encrypt')}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={attachPublicKey}
                onChange={(e) => setAttachPublicKey(e.target.checked)}
              />
              {t(locale, 'mail.opengpg.attachPublicKey')}
            </label>
            {encryptMessage && recipientKeys.length > 0 ? (
              <ul className="space-y-2 text-xs text-muted-foreground">
                {recipientKeys.map((row) => (
                  <li key={row.email} className="space-y-1">
                    <div>{row.email}</div>
                    {row.keys.length === 0 ? (
                      <span className="text-destructive">
                        {t(locale, 'mail.opengpg.noRecipientKey')}
                      </span>
                    ) : row.ambiguous ? (
                      <select
                        className="h-8 w-full rounded-md border border-input bg-transparent px-2"
                        value={recipientKeyIds[row.email] ?? ''}
                        onChange={(e) =>
                          setRecipientKeyIds((prev) => ({
                            ...prev,
                            [row.email]: e.target.value,
                          }))
                        }
                      >
                        <option value="">{t(locale, 'mail.opengpg.pickRecipientKey')}</option>
                        {row.keys.map((k) => (
                          <option key={k.id} value={k.id}>
                            {k.primaryEmail} ({k.fingerprint.slice(0, 8)}…)
                          </option>
                        ))}
                      </select>
                    ) : (
                      <span>{t(locale, 'mail.opengpg.recipientKeyOk')}</span>
                    )}
                  </li>
                ))}
              </ul>
            ) : null}
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
