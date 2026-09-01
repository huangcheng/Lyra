/**
 * One message inside the reader's conversation stack.
 *
 * Owns its full-body fetch (list payloads carry no body), remote-content
 * gating, OpenGPG banner, and tracking-pixel advisory. Collapsed cards
 * render a single header row with a snippet; expanding selects the
 * message so the toolbar acts on it. Both states show a hover action
 * panel (trash / reply / reply-all / forward) when handlers are passed.
 */

import { format } from 'date-fns';
import { File, Forward, Paperclip, Reply, ReplyAll, Shield, Trash2 } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

import { OpengpgMessageBanner } from '@/components/mail/opengpg-message-banner';
import { DkimStatus } from '@/components/mail/dkim-status';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Button } from '@/components/ui/button';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { downloadAttachment, formatBytes, resolveInlineImages } from '@/lib/attachments';
import { useAvatar } from '@/lib/avatar';
import { MARK_READ_OPEN_DWELL_MS } from '@/lib/mark-read-policy';
import { markMessageReadOnServer } from '@/lib/mark-message-read';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { allowSenderPrivacy } from '@/lib/privacy-api';
import { sanitizeEmailHtml } from '@/lib/sanitize-email-html';
import { cn, getInitials, avatarTone } from '@/lib/utils';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

interface MessageCardProps {
  messageId: string;
  expanded: boolean;
  /** Hide the subject line (redundant when the stack has a shared title). */
  hideSubject?: boolean;
  onToggle: () => void;
  /** Hover-panel actions; all three must be present for the panel to render. */
  onReply?: (all: boolean) => void;
  onForward?: () => void;
  onTrash?: () => void;
}

export function MessageCard({
  messageId,
  expanded,
  hideSubject,
  onToggle,
  onReply,
  onForward,
  onTrash,
}: MessageCardProps) {
  const locale = useUIStore((s) => s.locale);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const token = useAuthStore((s) => s.token);
  const mail = useMailStore((s) => s.messages[messageId]);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);

  const [allowRemoteContent, setAllowRemoteContent] = useState(false);
  const [bodyLoading, setBodyLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const mailBodyRef = useRef<HTMLDivElement>(null);
  const autoMarkedRef = useRef(false);
  const avatarUrl = useAvatar(mail?.from.email);

  // Always fetch the detail payload on expand (mirrors the old reader):
  // only GET /messages/:id sets remoteContentBlocked / opengpg, and list
  // payloads carry no body. Refetch once when remote content is allowed.
  const fetchedWithRemoteRef = useRef(false);
  const fetchedDetailRef = useRef(false);
  useEffect(() => {
    if (!expanded || !token) return;
    if (fetchedDetailRef.current && !(allowRemoteContent && !fetchedWithRemoteRef.current)) return;
    let cancelled = false;
    const load = async () => {
      setLoadError(null);
      setBodyLoading(true);
      try {
        const qs = allowRemoteContent ? '?remote_content=allow' : '';
        const msg = await api<ApiMessage>(`/messages/${messageId}${qs}`);
        if (cancelled) return;
        fetchedDetailRef.current = true;
        if (allowRemoteContent) fetchedWithRemoteRef.current = true;
        upsertMessage(mapApiMessage(msg));
      } catch (err: unknown) {
        if (!cancelled) setLoadError(err instanceof Error ? err.message : 'Failed to load message');
      } finally {
        if (!cancelled) setBodyLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [expanded, messageId, token, upsertMessage, allowRemoteContent, reloadNonce]);

  // on_open mark-read for expanded cards. The selected message is already
  // handled by MailDisplay (both policies), so skip it here.
  const isSelected = useUIStore((s) => s.selectedMessageId === messageId);
  useEffect(() => {
    if (!expanded || isSelected || !mail || mail.isRead || markReadPolicy === 'manual') return;
    if (autoMarkedRef.current) return;
    const timer = window.setTimeout(() => {
      autoMarkedRef.current = true;
      void markMessageReadOnServer(messageId).then((ok) => {
        if (ok) markMessageRead(messageId);
      });
    }, MARK_READ_OPEN_DWELL_MS);
    return () => window.clearTimeout(timer);
  }, [expanded, isSelected, mail, markReadPolicy, messageId, markMessageRead]);

  // Sanitize the body, then resolve inline cid: parts against the detail
  // payload's attachment metadata (bytes go through apiBlob for auth).
  // Resolved HTML is keyed by its source string and applied during render,
  // so state is only ever set from the async resolution — never synchronously.
  const bodyHtml = mail?.bodyHtml ?? null;
  const bodyAttachments = mail?.attachments;
  const [resolvedBody, setResolvedBody] = useState<{ html: string; source: string } | null>(null);
  useEffect(() => {
    if (!bodyHtml) return;
    let revoke: (() => void) | null = null;
    let cancelled = false;
    const sanitized = sanitizeEmailHtml(bodyHtml);
    void resolveInlineImages(sanitized, bodyAttachments).then((resolved) => {
      if (cancelled) {
        resolved.revoke();
        return;
      }
      revoke = resolved.revoke;
      setResolvedBody({ html: resolved.html, source: bodyHtml });
    });
    return () => {
      cancelled = true;
      if (revoke) revoke();
    };
  }, [bodyHtml, bodyAttachments]);
  const renderHtml = resolvedBody && resolvedBody.source === bodyHtml ? resolvedBody.html : null;

  // Tracking-pixel advisory on the rendered body. The advisory resets
  // when the rendered body changes (keyed during render), and the effect
  // only scans/subscribes — state flips happen from image-load callbacks.
  const [pixelAdvisory, setPixelAdvisory] = useState(false);
  const [advisoryFor, setAdvisoryFor] = useState<string | null>(renderHtml);
  if (renderHtml !== advisoryFor) {
    setAdvisoryFor(renderHtml);
    setPixelAdvisory(false);
  }
  useEffect(() => {
    if (!expanded || !bodyHtml) return;
    const root = mailBodyRef.current;
    if (!root) return;

    const markIfPixel = (img: HTMLImageElement) => {
      if (img.getAttribute('data-lyra-pixel') === '1') {
        setPixelAdvisory(true);
        return;
      }
      if (img.complete && img.naturalWidth > 0 && img.naturalWidth <= 4 && img.naturalHeight <= 4) {
        img.setAttribute('data-lyra-pixel', '1');
        setPixelAdvisory(true);
      }
    };

    const onLoad = (ev: Event) => {
      const target = ev.target;
      if (target instanceof HTMLImageElement) markIfPixel(target);
    };

    root.querySelectorAll('img').forEach((img) => markIfPixel(img));
    root.addEventListener('load', onLoad, true);
    return () => root.removeEventListener('load', onLoad, true);
  }, [expanded, bodyHtml, renderHtml, allowRemoteContent]);

  if (!mail) return null;

  const fromLabel = mail.from.name ?? mail.from.email;
  const snippet = (mail.snippet || mail.bodyText || '').replace(/\s+/g, ' ').trim();
  const visibleAttachments = (mail.attachments ?? []).filter((a) => !a.isInline);

  const handleHeaderClick = () => {
    setSelectedMessage(messageId);
    onToggle();
  };

  // Apple Mail-style hover actions: trash / reply / reply-all / forward on
  // this message. Revealed on card hover or keyboard focus within the card.
  const hoverPanel =
    onReply && onForward && onTrash ? (
      <div
        role="toolbar"
        aria-label={t(locale, 'mail.messageActions')}
        className="hidden items-center gap-0.5 rounded-[7px] border border-input bg-card p-0.5 shadow-whisper group-focus-within:flex group-hover:flex"
        onClick={(e) => e.stopPropagation()}
      >
        {(
          [
            { icon: Trash2, label: t(locale, 'mail.moveToTrash'), action: onTrash },
            { icon: Reply, label: t(locale, 'mail.reply'), action: () => onReply(false) },
            { icon: ReplyAll, label: t(locale, 'mail.replyAll'), action: () => onReply(true) },
            { icon: Forward, label: t(locale, 'mail.forward'), action: onForward },
          ] as const
        ).map(({ icon: Icon, label, action }) => (
          <button
            key={label}
            type="button"
            title={label}
            aria-label={label}
            onClick={action}
            className="flex size-7 items-center justify-center rounded-[5px] text-ter-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <Icon className="size-3.5" />
          </button>
        ))}
      </div>
    ) : null;

  if (!expanded) {
    return (
      <div className="group relative">
        <button
          type="button"
          onClick={handleHeaderClick}
          className="flex w-full items-center gap-3 rounded-lg border border-border bg-card px-3 py-2.5 text-left text-sm shadow-whisper transition-colors hover:bg-accent/50"
        >
          <span className="flex w-2.5 shrink-0 justify-center">
            {!mail.isRead ? <span className="size-1.5 rounded-full bg-unread" aria-hidden /> : null}
          </span>
          <Avatar className="h-7 w-7 shrink-0">
            <AvatarImage src={avatarUrl ?? undefined} alt={fromLabel} />
            <AvatarFallback className={cn('text-[11px]', avatarTone(fromLabel))}>
              {getInitials(fromLabel)}
            </AvatarFallback>
          </Avatar>
          <span className={cn('shrink-0', !mail.isRead && 'font-semibold')}>{fromLabel}</span>
          <span className="min-w-0 flex-1 truncate text-muted-foreground">{snippet}</span>
          <span className="shrink-0 text-[11px] tabular-nums text-ter-foreground">
            {format(new Date(mail.date), 'MMM d, h:mm a')}
          </span>
        </button>
        {hoverPanel ? (
          <div className="absolute top-1/2 right-2 -translate-y-1/2">{hoverPanel}</div>
        ) : null}
      </div>
    );
  }

  const showRemoteBanner = Boolean(mail.remoteContentBlocked) && !allowRemoteContent;

  return (
    <article className="group relative overflow-hidden rounded-xl border border-border bg-card shadow-whisper">
      {hoverPanel ? <div className="absolute top-3.5 right-4 z-10">{hoverPanel}</div> : null}
      <button
        type="button"
        onClick={handleHeaderClick}
        className="flex w-full items-start gap-4 px-4 pt-4 pb-3 text-left text-sm"
      >
        <Avatar className="h-10 w-10 shrink-0">
          <AvatarImage src={avatarUrl ?? undefined} alt={fromLabel} />
          <AvatarFallback className={avatarTone(fromLabel)}>
            {getInitials(fromLabel)}
          </AvatarFallback>
        </Avatar>
        <div className="grid min-w-0 flex-1 gap-1">
          <div className={cn('font-semibold', !mail.isRead && 'text-foreground')}>
            {!mail.isRead ? (
              <span className="mr-1.5 inline-block size-1.5 rounded-full bg-unread align-middle" />
            ) : null}
            {fromLabel}
          </div>
          {!hideSubject ? (
            <div className="line-clamp-1 text-[13px] font-medium">{mail.subject}</div>
          ) : null}
          <div className="line-clamp-1 text-xs text-muted-foreground">
            <span className="font-medium">{t(locale, 'mail.to')}:</span>{' '}
            {mail.to[0]?.name ?? mail.to[0]?.email ?? mail.from.email}
          </div>
        </div>
        {mail.date ? (
          <div className="shrink-0 text-xs text-muted-foreground">
            {format(new Date(mail.date), 'PPpp')}
          </div>
        ) : null}
      </button>
      {mail.opengpg ? (
        <OpengpgMessageBanner
          locale={locale}
          status={mail.opengpg}
          onUnlocked={() => setReloadNonce((n) => n + 1)}
        />
      ) : null}
      {showRemoteBanner ? (
        <div className="px-4 pb-1">
          <div className="flex items-center gap-2 rounded-lg border border-border px-3.5 py-2.5 text-[12.5px] text-muted-foreground">
            <Shield className="size-3.5 shrink-0" aria-hidden />
            <span className="min-w-0 flex-1">{t(locale, 'mail.remoteContentHidden')}</span>
            <button
              type="button"
              className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-foreground/80"
              onClick={() => setAllowRemoteContent(true)}
            >
              {t(locale, 'mail.showRemoteContent')}
            </button>
            <button
              type="button"
              className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-foreground/80"
              onClick={() => {
                void allowSenderPrivacy(mail.from.email)
                  .then(() => setAllowRemoteContent(true))
                  .catch(() => {});
              }}
            >
              {t(locale, 'mail.alwaysShowFromSender')}
            </button>
          </div>
        </div>
      ) : null}
      {pixelAdvisory && !showRemoteBanner ? (
        <div className="px-4 pb-1">
          <div
            className="flex items-center gap-2 rounded-lg border border-border/80 bg-muted/40 px-3.5 py-2 text-[12px] text-ter-foreground"
            role="status"
          >
            <Shield className="size-3.5 shrink-0 opacity-70" aria-hidden />
            <span className="min-w-0 flex-1">{t(locale, 'mail.trackingPixelHint')}</span>
          </div>
        </div>
      ) : null}
      {mail.dkim ? (
        <div className="px-4 pb-1">
          <DkimStatus dkim={mail.dkim} locale={locale} />
        </div>
      ) : null}
      <div className="px-4 pt-1 pb-4 text-sm">
        {loadError ? (
          <div className="flex flex-col items-start gap-3">
            <p className="whitespace-pre-wrap text-destructive">{loadError}</p>
            <Button variant="outline" size="sm" onClick={() => setReloadNonce((n) => n + 1)}>
              {t(locale, 'common.retry')}
            </Button>
          </div>
        ) : bodyLoading && !mail.bodyHtml && !mail.bodyText ? (
          <div className="space-y-3 py-1" aria-hidden>
            <div className="h-3.5 w-2/3 animate-pulse rounded bg-muted" />
            <div className="h-3.5 w-full animate-pulse rounded bg-muted" />
            <div className="h-3.5 w-5/6 animate-pulse rounded bg-muted" />
          </div>
        ) : mail.bodyHtml ? (
          <div
            ref={mailBodyRef}
            className="mail-body animate-in fade-in duration-150"
            // Sanitized via sanitizeEmailHtml (class/style-tag stripped);
            // inline cid: images resolved to object URLs after sanitize.
            dangerouslySetInnerHTML={{
              __html: renderHtml ?? sanitizeEmailHtml(mail.bodyHtml),
            }}
          />
        ) : mail.bodyText ? (
          <div className="whitespace-pre-wrap">{mail.bodyText}</div>
        ) : (
          <div className="flex flex-col items-start gap-3 text-muted-foreground">
            <p>{t(locale, 'mail.bodyUnavailable')}</p>
            <Button variant="outline" size="sm" onClick={() => setReloadNonce((n) => n + 1)}>
              {t(locale, 'common.retry')}
            </Button>
          </div>
        )}
        {visibleAttachments.length > 0 ? (
          <div className="mt-4 space-y-1.5">
            <div className="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-ter-foreground">
              <Paperclip className="size-3" aria-hidden />
              {t(locale, 'mail.attachments')}
            </div>
            <ul className="flex flex-wrap gap-1.5">
              {visibleAttachments.map((att) => (
                <li key={att.id}>
                  <button
                    type="button"
                    title={att.filename}
                    onClick={() =>
                      void downloadAttachment(att).catch(() =>
                        setAttachmentError(t(locale, 'mail.attachmentDownloadError')),
                      )
                    }
                    className="flex max-w-xs items-center gap-2 rounded-md border border-border bg-muted/40 px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-accent"
                  >
                    <File className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                    <span className="min-w-0">
                      <span className="block truncate font-medium">
                        {att.filename || t(locale, 'mail.attachmentUnnamed')}
                      </span>
                      {att.sizeBytes != null ? (
                        <span className="block text-ter-foreground">
                          {formatBytes(att.sizeBytes)}
                        </span>
                      ) : null}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {attachmentError ? <p className="text-xs text-destructive">{attachmentError}</p> : null}
          </div>
        ) : null}
      </div>
    </article>
  );
}
