/**
 * Compose dialog for writing new emails.
 */

import { useEffect, useMemo, useRef, useState } from 'react';

import {
  ChevronDown,
  KeyRound,
  Lock,
  Paperclip,
  PenLine,
  SendHorizontal,
  Trash2,
  Type,
  X,
} from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogTitle } from '@/components/ui/dialog';
import { Textarea } from '@/components/ui/textarea';
import { t } from '@/i18n';
import { RecipientsInput } from '@/components/compose/recipients-input';
import { splitAddresses } from '@/lib/addresses';
import { RichTextEditor } from '@/components/compose/rich-text-editor';
import { api, apiBlob } from '@/lib/api-client';
import { formatBytes } from '@/lib/attachments';
import { signatureHtml } from '@/lib/compose-html';
import { htmlToText } from '@/lib/html-text';
import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { cn } from '@/lib/utils';
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
  /** Committed address pills; form.to/cc/bcc hold the pending typed text. */
  const [toChips, setToChips] = useState<string[]>([]);
  const [ccChips, setCcChips] = useState<string[]>([]);
  const [bccChips, setBccChips] = useState<string[]>([]);
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
  const [showCc, setShowCc] = useState(false);
  const [showBcc, setShowBcc] = useState(false);
  const [cryptoOpen, setCryptoOpen] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  /** Last autosave payload sent — prevents re-saving unchanged drafts. */
  const autosavePayloadRef = useRef<string | null>(null);
  /** Forward drafts load their original attachments once, not per rerender. */
  const forwardLoadedRef = useRef<unknown>(null);
  /** The draft this open session seeded from — guards against re-seeding on
   * store identity churn (accounts/folders refresh on every sync tick). */
  const seededForRef = useRef<unknown>(null);

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
      splitAddresses(value.toLowerCase()).filter((e) => e.includes('@'));
    return [
      ...toChips,
      ...split(form.to),
      ...ccChips,
      ...split(form.cc),
      ...bccChips,
      ...split(form.bcc),
    ];
  }, [toChips, ccChips, bccChips, form.to, form.cc, form.bcc]);

  useEffect(() => {
    if (!composeOpen) {
      seededForRef.current = null;
      return;
    }
    // Seed once per open: the accounts/folders arrays get fresh identities on
    // every sync tick, and depending on them here would wipe the half-written
    // draft mid-compose (observed live: autosave → push → form reset).
    if (seededForRef.current === composeDraft) return;
    seededForRef.current = composeDraft;
    const effectiveFrom =
      selectedAccountId === ALL_ACCOUNTS ? (accounts[0]?.id ?? '') : selectedAccountId;
    setForm({
      to: '',
      cc: '',
      bcc: '',
      subject: composeDraft?.subject ?? '',
      body: composeDraft?.body ?? '',
    });
    setToChips(splitAddresses(composeDraft?.to ?? ''));
    setCcChips(splitAddresses(composeDraft?.cc ?? ''));
    setBccChips([]);
    setFromAccountId(effectiveFrom);
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
    setShowCc(Boolean(composeDraft?.cc));
    setShowBcc(false);
    setCryptoOpen(false);
    // Reply/forward/draft pass their own initialHtml; new mail seeds the
    // from-account's signature (empty string when none is configured).
    const seeded =
      composeDraft?.initialHtml ??
      signatureHtml(accounts.find((a) => a.id === effectiveFrom)?.signature);
    setEditorHtml(seeded);
    setInitialHtml(seeded);
    setEditorKey((k) => k + 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [composeOpen, composeDraft]);

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
  const fullTo = useMemo(() => [...toChips, ...splitAddresses(form.to)], [toChips, form.to]);
  const fullCc = useMemo(() => [...ccChips, ...splitAddresses(form.cc)], [ccChips, form.cc]);
  const fullBcc = useMemo(() => [...bccChips, ...splitAddresses(form.bcc)], [bccChips, form.bcc]);
  const currentBodyText = richMode ? htmlToText(editorHtml ?? '') : form.body;
  const currentBodyHtml = richMode ? editorHtml || undefined : undefined;
  const draftDirty =
    fullTo.length > 0 || form.subject.trim() !== '' || currentBodyText.trim() !== '';
  const autosavePayload = JSON.stringify({
    accountId: fromAccountId,
    to: fullTo,
    cc: fullCc,
    subject: form.subject,
    body: currentBodyText,
    bodyHtml: currentBodyHtml,
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
              to: fullTo.map((email) => ({ email })),
              cc: fullCc.map((email) => ({ email })),
              subject: form.subject,
              bodyText: currentBodyText,
              bodyHtml: currentBodyHtml,
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

  // Emails join to a stable key: re-lookup only when the recipient set
  // actually changes, not when the source arrays get new identities.
  const recipientKey = recipientEmails.join(',');
  useEffect(() => {
    if (!composeOpen || !encryptMessage || recipientKey === '') {
      setRecipientKeys([]);
      return;
    }
    let cancelled = false;
    void lookupRecipientKeys(recipientKey.split(','))
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
  }, [composeOpen, encryptMessage, recipientKey]);

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
    setToChips([]);
    setCcChips([]);
    setBccChips([]);
    setError(null);
    setSuccess(false);
    setSignMessage(false);
    setEncryptMessage(false);
    setAttachPublicKey(false);
    setFiles([]);
    setDraftMessageId(null);
    setDraftSavedAt(null);
    setRichMode(true);
    setShowCc(false);
    setShowBcc(false);
    setCryptoOpen(false);
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
    if (fullTo.length === 0) {
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
        to: fullTo.map((email) => ({ email })),
        cc: fullCc.map((email) => ({ email })),
        bcc: fullBcc.map((email) => ({ email })),
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

  const cryptoActive = signMessage || encryptMessage || attachPublicKey;

  const iconBtn =
    'relative flex size-8 items-center justify-center rounded-md text-muted-foreground transition-all hover:bg-accent hover:text-foreground active:scale-[0.96] disabled:pointer-events-none disabled:opacity-50';

  return (
    <Dialog open={composeOpen} onOpenChange={(open) => !open && handleClose()}>
      <DialogContent
        className="flex h-[min(720px,calc(100dvh-2rem))] flex-col gap-0 overflow-hidden rounded-xl p-0 shadow-2xl sm:max-w-3xl"
        showCloseButton={false}
      >
        {/* Title bar */}
        <div className="flex h-11 shrink-0 items-center gap-3 border-b border-border/60 pr-2 pl-4">
          <DialogTitle className="text-[13px] font-medium text-muted-foreground">
            {t(locale, titleKey)}
          </DialogTitle>
          <Button
            variant="ghost"
            size="icon-sm"
            className="ml-auto text-muted-foreground hover:text-foreground"
            aria-label={t(locale, 'mail.close')}
            onClick={handleClose}
          >
            <X className="size-4" aria-hidden />
          </Button>
        </div>

        {/* Address block */}
        <div className="shrink-0 px-4">
          {accounts.length > 1 || selectedAccountId === ALL_ACCOUNTS ? (
            <div className="flex h-11 items-center gap-3 border-b border-border/60">
              <label htmlFor="compose-from" className="w-11 shrink-0 text-xs text-muted-foreground">
                {t(locale, 'mail.from')}
              </label>
              <div className="relative min-w-0 flex-1">
                <select
                  id="compose-from"
                  className="h-11 w-full appearance-none bg-transparent pr-6 text-sm outline-none"
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
                      {a.displayName && a.displayName !== a.emailAddress
                        ? ` (${a.displayName})`
                        : ''}
                    </option>
                  ))}
                </select>
                <ChevronDown
                  className="pointer-events-none absolute top-1/2 right-1 size-3.5 -translate-y-1/2 text-muted-foreground"
                  aria-hidden
                />
              </div>
            </div>
          ) : null}
          <div className="flex items-center gap-3 border-b border-border/60 pr-1">
            <label
              htmlFor="compose-to"
              className="w-11 shrink-0 self-center py-2 text-xs text-muted-foreground"
            >
              {t(locale, 'mail.to')}
            </label>
            <div className="min-w-0 flex-1">
              <RecipientsInput
                id="compose-to"
                chips={toChips}
                input={form.to}
                onChipsChange={setToChips}
                onInputChange={(v) => setForm((f) => ({ ...f, to: v }))}
                placeholder={t(locale, 'mail.toPlaceholder')}
                autoFocus
              />
            </div>
            {!showCc ? (
              <button
                type="button"
                className="rounded px-1.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                onClick={() => setShowCc(true)}
              >
                {t(locale, 'mail.cc')}
              </button>
            ) : null}
            {!showBcc ? (
              <button
                type="button"
                className="rounded px-1.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                onClick={() => setShowBcc(true)}
              >
                {t(locale, 'mail.bcc')}
              </button>
            ) : null}
          </div>
          {showCc ? (
            <div className="flex items-center gap-3 border-b border-border/60 pr-1">
              <label
                htmlFor="compose-cc"
                className="w-11 shrink-0 self-center py-2 text-xs text-muted-foreground"
              >
                {t(locale, 'mail.cc')}
              </label>
              <div className="min-w-0 flex-1">
                <RecipientsInput
                  id="compose-cc"
                  chips={ccChips}
                  input={form.cc}
                  onChipsChange={setCcChips}
                  onInputChange={(v) => setForm((f) => ({ ...f, cc: v }))}
                />
              </div>
            </div>
          ) : null}
          {showBcc ? (
            <div className="flex items-center gap-3 border-b border-border/60 pr-1">
              <label
                htmlFor="compose-bcc"
                className="w-11 shrink-0 self-center py-2 text-xs text-muted-foreground"
              >
                {t(locale, 'mail.bcc')}
              </label>
              <div className="min-w-0 flex-1">
                <RecipientsInput
                  id="compose-bcc"
                  chips={bccChips}
                  input={form.bcc}
                  onChipsChange={setBccChips}
                  onInputChange={(v) => setForm((f) => ({ ...f, bcc: v }))}
                />
              </div>
            </div>
          ) : null}
          <div className="flex h-11 items-center border-b border-border/60">
            <input
              id="compose-subject"
              className="h-full min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground/60"
              value={form.subject}
              onChange={(e) => setForm((f) => ({ ...f, subject: e.target.value }))}
              placeholder={t(locale, 'mail.subjectPlaceholder')}
              aria-label={t(locale, 'mail.subject')}
            />
          </div>
        </div>

        {/* Body */}
        <div className="flex min-h-0 flex-1 flex-col">
          {richMode ? (
            <RichTextEditor
              key={editorKey}
              className="flex min-h-0 flex-1 flex-col rounded-none border-0"
              toolbarPosition="bottom"
              contentClassName="max-h-none min-h-56 flex-1 px-4 py-3"
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
              className="min-h-56 flex-1 resize-none rounded-none border-0 px-4 py-3 shadow-none focus-visible:border-transparent focus-visible:ring-0"
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
        </div>
        {/* Attachment chips */}
        {files.length > 0 ? (
          <div className="flex max-h-28 shrink-0 flex-wrap items-center gap-1.5 overflow-y-auto border-t border-border/60 px-4 py-2.5">
            {files.map((f, i) => (
              <span
                key={`${f.name}-${i}`}
                className="flex max-w-56 items-center gap-1.5 rounded-full border border-border/70 bg-muted/40 py-1 pr-1 pl-2.5 text-xs"
              >
                <Paperclip className="size-3 shrink-0 text-muted-foreground" aria-hidden />
                <span className="min-w-0 truncate font-medium">{f.name}</span>
                <span className="shrink-0 text-muted-foreground tabular-nums">
                  {formatBytes(f.size)}
                </span>
                <button
                  type="button"
                  aria-label={t(locale, 'mail.removeAttachment', { name: f.name })}
                  className="relative flex size-5 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors before:absolute before:-inset-1 before:content-[''] hover:bg-accent hover:text-foreground"
                  onClick={() => removeFile(i)}
                >
                  <X className="size-3" aria-hidden />
                </button>
              </span>
            ))}
          </div>
        ) : null}
        {/* OpenGPG panel */}
        {cryptoOpen ? (
          <div className="shrink-0 space-y-2.5 border-t border-border/60 bg-muted/30 px-4 py-3">
            <div className="flex items-center gap-2">
              <Lock className="size-3.5 text-muted-foreground" aria-hidden />
              <p className="text-xs font-medium">{t(locale, 'mail.opengpg.composeTitle')}</p>
            </div>
            {!cryptoAllowed ? (
              <p className="text-xs text-muted-foreground">
                {t(locale, 'mail.opengpg.needsAccountKey')}
              </p>
            ) : null}
            <div className="flex flex-wrap items-center gap-1.5">
              {(
                [
                  {
                    on: signMessage,
                    toggle: () => setSignMessage((v) => !v),
                    icon: PenLine,
                    label: t(locale, 'mail.opengpg.sign'),
                  },
                  {
                    on: encryptMessage,
                    toggle: () => setEncryptMessage((v) => !v),
                    icon: Lock,
                    label: t(locale, 'mail.opengpg.encrypt'),
                  },
                  {
                    on: attachPublicKey,
                    toggle: () => setAttachPublicKey((v) => !v),
                    icon: KeyRound,
                    label: t(locale, 'mail.opengpg.attachPublicKey'),
                  },
                ] as const
              ).map(({ on, toggle, icon: Icon, label }) => (
                <button
                  key={label}
                  type="button"
                  aria-pressed={on}
                  disabled={!cryptoAllowed}
                  onClick={toggle}
                  className={cn(
                    'flex items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-all active:scale-[0.97] disabled:pointer-events-none disabled:opacity-40',
                    on
                      ? 'border-foreground/25 bg-foreground/[0.06] text-foreground'
                      : 'border-border/70 text-muted-foreground hover:border-foreground/20 hover:text-foreground',
                  )}
                >
                  <Icon className="size-3.5" aria-hidden />
                  {label}
                </button>
              ))}
            </div>
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
          </div>
        ) : null}
        {error ? (
          <div className="shrink-0 border-t border-destructive/25 bg-destructive/5 px-4 py-2 text-xs text-destructive">
            {error}
          </div>
        ) : null}

        {/* Footer bar */}
        <div className="flex h-14 shrink-0 items-center gap-1.5 border-t border-border/60 px-3">
          <Button
            type="button"
            className="h-9 rounded-full bg-primary px-4 text-primary-foreground transition-all hover:bg-primary/90 active:scale-[0.97]"
            onClick={() => void handleSend()}
            disabled={sending}
            title={t(locale, 'mail.sendShortcut')}
          >
            <SendHorizontal className="size-3.5" aria-hidden />
            {sending ? t(locale, 'mail.sending') : t(locale, 'mail.send')}
          </Button>
          {success ? (
            <span className="ml-1 text-xs text-muted-foreground">
              {t(locale, 'mail.sendSuccess')}
            </span>
          ) : (
            <span className="ml-1 hidden text-[11px] text-muted-foreground md:inline">
              {t(locale, 'mail.sendShortcut')}
            </span>
          )}
          <div className="ml-auto flex items-center gap-0.5">
            {draftSavedAt && !success ? (
              <span className="mr-1.5 text-[11px] text-muted-foreground tabular-nums">
                {t(locale, 'mail.draftSaved')}
              </span>
            ) : null}
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
            <button
              type="button"
              className={iconBtn}
              disabled={sending || files.length >= MAX_ATTACHMENTS}
              onClick={() => fileInputRef.current?.click()}
              aria-label={t(locale, 'mail.addAttachment')}
            >
              <Paperclip className="size-4" aria-hidden />
              {files.length > 0 ? (
                <span className="absolute top-0.5 right-0.5 flex size-3.5 items-center justify-center rounded-full bg-foreground text-[9px] font-medium text-background tabular-nums">
                  {files.length}
                </span>
              ) : null}
            </button>
            <button
              type="button"
              className={cn(iconBtn, !richMode && 'bg-accent text-foreground')}
              aria-pressed={!richMode}
              title={t(locale, richMode ? 'mail.plainText' : 'mail.richText')}
              aria-label={t(locale, richMode ? 'mail.plainText' : 'mail.richText')}
              onClick={() => {
                if (richMode) {
                  // Rich → plain carries the text content over.
                  setForm((f) => ({ ...f, body: htmlToText(editorHtml ?? '') || f.body }));
                  setRichMode(false);
                } else {
                  setRichMode(true);
                }
              }}
            >
              <Type className="size-4" aria-hidden />
            </button>
            <button
              type="button"
              className={cn(iconBtn, (cryptoOpen || cryptoActive) && 'bg-accent text-foreground')}
              aria-pressed={cryptoOpen}
              title={t(locale, 'mail.opengpg.composeTitle')}
              aria-label={t(locale, 'mail.opengpg.composeTitle')}
              onClick={() => setCryptoOpen((v) => !v)}
            >
              <Lock className="size-4" aria-hidden />
              {cryptoActive ? (
                <span
                  className="absolute top-1.5 right-1.5 size-1.5 rounded-full bg-foreground"
                  aria-hidden
                />
              ) : null}
            </button>
            <span className="mx-1 h-4 w-px bg-border" aria-hidden />
            <button
              type="button"
              className={cn(iconBtn, 'hover:text-destructive')}
              onClick={handleDiscard}
              disabled={sending}
              aria-label={t(locale, 'mail.discard')}
              title={t(locale, 'mail.discard')}
            >
              <Trash2 className="size-4" aria-hidden />
            </button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
