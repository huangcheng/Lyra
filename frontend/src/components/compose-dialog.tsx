/**
 * Compose dialog for writing new emails.
 */

import { useEffect, useMemo, useRef, useState } from 'react';

import { Paperclip, Type, X } from 'lucide-react';

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
import { RichTextEditor } from '@/components/compose/rich-text-editor';
import { api, apiBlob } from '@/lib/api-client';
import { formatBytes } from '@/lib/attachments';
import { htmlToText } from '@/lib/html-text';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import {
  listOpengpgKeys,
  lookupRecipientKeys,
  type OpengpgKey,
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

/** Mirrors the backend's per-file cap and count limit (LYRA_MAX_ATTACHMENT_BYTES). */
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const MAX_ATTACHMENTS = 10;

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
  const [keys, setKeys] = useState<OpengpgKey[] | null>(null);
  const [files, setFiles] = useState<File[]>([]);
  /** Rich editor output (HTML); plaintext mode uses form.body instead. */
  const [richMode, setRichMode] = useState(true);
  const [editorHtml, setEditorHtml] = useState('');
  const [editorKey, setEditorKey] = useState(0);
  const [initialHtml, setInitialHtml] = useState('');
  const [draftMessageId, setDraftMessageId] = useState<string | null>(null);
  const [draftSavedAt, setDraftSavedAt] = useState<number | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  /** Last autosave payload sent — prevents re-saving unchanged drafts. */
  const autosavePayloadRef = useRef<string | null>(null);
  /** Forward drafts load their original attachments once, not per rerender. */
  const forwardLoadedRef = useRef<unknown>(null);

  /** The From account owns a secret (identity) key → sign/encrypt available. */
  const fromAccountHasKey = useMemo(
    () =>
      Boolean(fromAccountId) &&
      (keys ?? []).some((k) => k.isSecret && k.accountId === fromAccountId),
    [keys, fromAccountId],
  );
  const cryptoAllowed = keys !== null && fromAccountHasKey;

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
      cc: composeDraft?.cc ?? '',
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
    setFiles([]);
    setDraftMessageId(composeDraft?.draftMessageId ?? null);
    setDraftSavedAt(null);
    setRichMode(true);
    setEditorHtml(composeDraft?.initialHtml ?? '');
    setInitialHtml(composeDraft?.initialHtml ?? '');
    setEditorKey((k) => k + 1);
  }, [composeOpen, composeDraft, selectedAccountId, accounts]);

  // Forwarding carries the original's (non-inline) attachments: fetch bytes
  // once per draft and seed the file list.
  useEffect(() => {
    if (!composeOpen || composeDraft?.mode !== 'forward') return;
    if (forwardLoadedRef.current === composeDraft) return;
    forwardLoadedRef.current = composeDraft;
    const originals = composeDraft.forwardAttachments ?? [];
    if (originals.length === 0) return;
    let cancelled = false;
    void (async () => {
      const loaded: File[] = [];
      for (const att of originals) {
        try {
          const blob = await apiBlob(`/attachments/${att.id}/download`);
          loaded.push(
            new File([blob], att.filename || 'attachment', {
              type: att.contentType || 'application/octet-stream',
            }),
          );
        } catch {
          // Broken originals are skipped; the send still goes out.
        }
      }
      if (!cancelled && loaded.length > 0) {
        setFiles((prev) => [...loaded, ...prev].slice(0, MAX_ATTACHMENTS));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [composeOpen, composeDraft]);

  function addFiles(list: FileList | null) {
    if (!list || list.length === 0) return;
    const incoming = Array.from(list);
    const tooBig = incoming.filter((f) => f.size > MAX_ATTACHMENT_BYTES);
    if (tooBig.length > 0) {
      setError(t(locale, 'mail.attachmentTooLarge', { name: tooBig[0].name }));
    }
    const ok = incoming.filter((f) => f.size <= MAX_ATTACHMENT_BYTES);
    setFiles((prev) => {
      if (prev.length + ok.length > MAX_ATTACHMENTS) {
        setError(t(locale, 'mail.attachmentCountLimit', { count: String(MAX_ATTACHMENTS) }));
        return [...prev, ...ok].slice(0, MAX_ATTACHMENTS);
      }
      return [...prev, ...ok];
    });
  }

  function removeFile(index: number) {
    setFiles((prev) => prev.filter((_, i) => i !== index));
  }

  useEffect(() => {
    if (!composeOpen) {
      setKeys(null);
      return;
    }
    let cancelled = false;
    void listOpengpgKeys()
      .then((rows) => {
        if (!cancelled) setKeys(rows);
      })
      .catch(() => {
        if (!cancelled) setKeys([]);
      });
    return () => {
      cancelled = true;
    };
  }, [composeOpen]);

  // Per-account identity model: without a key the crypto switches are inert.
  useEffect(() => {
    if (!cryptoAllowed) {
      setSignMessage(false);
      setEncryptMessage(false);
      setAttachPublicKey(false);
    }
  }, [cryptoAllowed]);

  // Debounced server autosave. Only when there is something to save, no
  // pending attachments (multipart sends skip drafts), and not mid-send.
  const draftDirty = form.to.trim() !== '' || form.subject.trim() !== '' || form.body.trim() !== '';
  const autosavePayload = JSON.stringify({
    accountId: fromAccountId,
    to: form.to,
    cc: form.cc,
    subject: form.subject,
    body: richMode ? htmlToText(editorHtml ?? '') : form.body,
    bodyHtml: richMode ? editorHtml : undefined,
    draftMessageId,
  });
  useEffect(() => {
    if (!composeOpen || !draftDirty || files.length > 0 || sending) return;
    if (autosavePayloadRef.current === autosavePayload) return;
    const timer = window.setTimeout(() => {
      autosavePayloadRef.current = autosavePayload;
      void (async () => {
        try {
          const res = await api<{ status: string; draftMessageId?: string | null }>('/drafts', {
            method: 'POST',
            body: JSON.stringify({
              accountId: fromAccountId,
              to: form.to
                .split(',')
                .map((x) => x.trim())
                .filter(Boolean)
                .map((email) => ({ email })),
              cc: form.cc
                .split(',')
                .map((x) => x.trim())
                .filter(Boolean)
                .map((email) => ({ email })),
              subject: form.subject,
              bodyText: form.body,
              existingDraftId: draftMessageId,
            }),
          });
          if (res.draftMessageId) setDraftMessageId(res.draftMessageId);
          setDraftSavedAt(Date.now());
        } catch {
          // Offline save failures surface on the next keystroke retry.
        }
      })();
    }, 1500);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [composeOpen, autosavePayload, files.length, sending]);

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
    setFiles([]);
    setDraftMessageId(null);
    setDraftSavedAt(null);
    setRichMode(true);
    setEditorHtml('');
    setInitialHtml('');
    autosavePayloadRef.current = null;
  };

  const handleDiscard = () => {
    if (draftMessageId) {
      void api(`/messages/${draftMessageId}/draft`, { method: 'DELETE' }).catch(() => {});
    }
    handleClose();
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
        cryptoAllowed && (signMessage || encryptMessage || attachPublicKey)
          ? {
              sign: signMessage,
              encrypt: encryptMessage,
              attachPublicKey,
              recipientKeyIds:
                Object.keys(recipientKeyIds).length > 0 ? recipientKeyIds : undefined,
            }
          : undefined;

      const bodyHtml = richMode ? editorHtml || null : null;
      const bodyText = richMode ? htmlToText(editorHtml ?? '') : form.body;
      const payload = {
        accountId: fromAccountId,
        to: split(form.to),
        cc: split(form.cc),
        bcc: split(form.bcc),
        subject: form.subject,
        bodyText,
        bodyHtml,
        opengpg,
      };

      if (files.length > 0) {
        // Multipart: `payload` JSON part + `files` parts (backend contract).
        const fd = new FormData();
        fd.append('payload', new Blob([JSON.stringify(payload)], { type: 'application/json' }));
        for (const f of files) fd.append('files', f, f.name);
        await api('/messages/send', { method: 'POST', body: fd });
      } else {
        await api('/messages/send', {
          method: 'POST',
          body: JSON.stringify(payload),
        });
      }

      if (draftMessageId) {
        void api(`/messages/${draftMessageId}/draft`, { method: 'DELETE' }).catch(() => {});
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
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden sm:max-w-xl"
        showCloseButton
      >
        <DialogHeader>
          <DialogTitle>{t(locale, titleKey)}</DialogTitle>
        </DialogHeader>
        <FieldGroup className="min-h-0 overflow-y-auto">
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
            <div className="flex items-center justify-between">
              <FieldLabel htmlFor="compose-body">{t(locale, 'mail.body')}</FieldLabel>
              <div className="flex items-center gap-0.5 rounded-md border border-input p-0.5">
                <button
                  type="button"
                  disabled={richMode}
                  className="rounded-[5px] px-2 py-0.5 text-[11px] font-medium disabled:bg-accent disabled:text-foreground"
                  onClick={() => setRichMode(true)}
                >
                  {t(locale, 'mail.richText')}
                </button>
                <button
                  type="button"
                  disabled={!richMode}
                  className="flex items-center gap-1 rounded-[5px] px-2 py-0.5 text-[11px] font-medium disabled:bg-accent disabled:text-foreground"
                  onClick={() => {
                    // Rich → plain carries the text content over.
                    setForm((f) => ({ ...f, body: htmlToText(editorHtml ?? '') || f.body }));
                    setRichMode(false);
                  }}
                >
                  <Type className="size-3" aria-hidden />
                  {t(locale, 'mail.plainText')}
                </button>
              </div>
            </div>
            {richMode ? (
              <RichTextEditor
                key={editorKey}
                initialHtml={initialHtml}
                onChange={setEditorHtml}
                placeholder={t(locale, 'mail.bodyPlaceholder')}
                onKeyDown={(e) => {
                  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
                    e.preventDefault();
                    void handleSend();
                  }
                }}
              />
            ) : (
              <Textarea
                id="compose-body"
                className="min-h-40"
                value={form.body}
                onChange={(e) => setForm((f) => ({ ...f, body: e.target.value }))}
                onKeyDown={(e) => {
                  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
                    e.preventDefault();
                    void handleSend();
                  }
                }}
                placeholder={t(locale, 'mail.bodyPlaceholder')}
              />
            )}
          </Field>
          <Field className="gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={sending || files.length >= MAX_ATTACHMENTS}
                onClick={() => fileInputRef.current?.click()}
              >
                <Paperclip className="size-3.5" />
                {t(locale, 'mail.addAttachment')}
              </Button>
              <input
                ref={fileInputRef}
                type="file"
                multiple
                className="hidden"
                onChange={(e) => {
                  addFiles(e.target.files);
                  e.target.value = '';
                }}
              />
              {files.length > 0
                ? files.map((f, i) => (
                    <span
                      key={`${f.name}-${i}`}
                      className="flex max-w-[200px] items-center gap-1.5 rounded-md border border-border bg-muted/40 px-2 py-1 text-xs"
                    >
                      <span className="min-w-0">
                        <span className="block truncate font-medium">{f.name}</span>
                        <span className="block text-ter-foreground">{formatBytes(f.size)}</span>
                      </span>
                      <button
                        type="button"
                        aria-label={t(locale, 'mail.removeAttachment', { name: f.name })}
                        className="shrink-0 rounded p-0.5 hover:bg-accent"
                        onClick={() => removeFile(i)}
                      >
                        <X className="size-3" aria-hidden />
                      </button>
                    </span>
                  ))
                : null}
            </div>
          </Field>
          <Field className="gap-3 rounded-md border border-border/60 p-3">
            <p className="text-sm font-medium">{t(locale, 'mail.opengpg.composeTitle')}</p>
            {!cryptoAllowed ? (
              <p className="text-xs text-muted-foreground">
                {t(locale, 'mail.opengpg.needsAccountKey')}
              </p>
            ) : null}
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="accent-foreground"
                checked={signMessage}
                disabled={!cryptoAllowed}
                onChange={(e) => setSignMessage(e.target.checked)}
              />
              {t(locale, 'mail.opengpg.sign')}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="accent-foreground"
                checked={encryptMessage}
                disabled={!cryptoAllowed}
                onChange={(e) => setEncryptMessage(e.target.checked)}
              />
              {t(locale, 'mail.opengpg.encrypt')}
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="accent-foreground"
                checked={attachPublicKey}
                disabled={!cryptoAllowed}
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
          {draftSavedAt && !success ? (
            <p className="text-xs text-ter-foreground">{t(locale, 'mail.draftSaved')}</p>
          ) : null}
          {success ? (
            <p className="text-sm text-muted-foreground">{t(locale, 'mail.sendSuccess')}</p>
          ) : null}
        </FieldGroup>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={handleDiscard} disabled={sending}>
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
