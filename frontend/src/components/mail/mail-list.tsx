/**
 * Message list — shadcn v3 mail example (All / Unread tabs live in the parent).
 */

import type { ComponentProps } from 'react';
import { useDraggable } from '@dnd-kit/core';
import { formatDistanceToNow, isSameDay, isSameMonth, subDays } from 'date-fns';
import { zhCN } from 'date-fns/locale';
import { Archive, CornerUpLeft, Inbox, Paperclip, SearchX, Trash2 } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { EmptyState } from '@/components/empty-state';
import { ErrorBanner, type ErrorBannerVariant } from '@/components/error-banner';
import { ConversationContextMenu } from '@/components/mail/conversation-context-menu';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { t } from '@/i18n';
import { ApiError, api } from '@/lib/api-client';
import { useAvatar } from '@/lib/avatar';
import { groupIntoConversations, type Conversation } from '@/lib/conversation';
import type { ConversationDragData } from '@/lib/conversation-actions';
import { fetchMessagesForView } from '@/lib/load-mail-messages';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { getInitials, avatarTone, cn } from '@/lib/utils';
import { syncEvents$ } from '@/rxjs/sync-events';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

function messageLabels(item: MailMessage): string[] {
  const labels: string[] = [];
  if (item.isStarred) labels.push('important');
  if (item.labels) {
    for (const label of item.labels) {
      if (!labels.includes(label)) labels.push(label);
    }
  }
  return labels;
}

type GroupKey = 'groupToday' | 'groupYesterday' | 'groupThisWeek' | 'groupThisMonth' | 'groupOlder';

/** Day-bucket key for a message date — drives the sticky list headers. */
function dayGroupKey(dateStr: string): GroupKey {
  const d = new Date(dateStr);
  const now = new Date();
  if (isSameDay(d, now)) return 'groupToday';
  if (isSameDay(d, subDays(now, 1))) return 'groupYesterday';
  if (d.getTime() > subDays(now, 7).getTime()) return 'groupThisWeek';
  if (isSameMonth(d, now)) return 'groupThisMonth';
  return 'groupOlder';
}

function getBadgeVariantFromLabel(label: string): ComponentProps<typeof Badge>['variant'] {
  if (label.toLowerCase() === 'work' || label.toLowerCase() === 'important') {
    return 'default';
  }
  if (label.toLowerCase() === 'personal') {
    return 'outline';
  }
  return 'secondary';
}

/** Sender avatar for a list row — hook lives here since rows render in a map. */
function ListAvatar({ email, label }: { email: string; label: string }) {
  const avatarUrl = useAvatar(email);
  return (
    <Avatar className="h-8 w-8 shrink-0">
      <AvatarImage src={avatarUrl ?? undefined} alt={label} />
      <AvatarFallback className={cn('text-xs', avatarTone(label))}>
        {getInitials(label)}
      </AvatarFallback>
    </Avatar>
  );
}

/** Draggable wrapper around a conversation row. */
function DraggableConversationRow({
  convo,
  children,
}: {
  convo: Conversation;
  children: React.ReactNode;
}) {
  const messageIds = convo.messages.map((m) => m.id);
  const folderIds = [...new Set(convo.messages.map((m) => m.folderId))];
  // No `attributes` spread: without a KeyboardSensor they would only add a
  // duplicate role="button" tab stop around the row's own interactive div.
  const { listeners, setNodeRef, isDragging } = useDraggable({
    id: `convo:${convo.key}`,
    data: {
      type: 'conversation',
      accountId: convo.latest.accountId,
      messageIds,
      folderIds,
      subject: convo.latest.subject,
      count: convo.messages.length,
    } satisfies ConversationDragData,
  });
  return (
    <div ref={setNodeRef} {...listeners} className={isDragging ? 'opacity-50' : undefined}>
      {children}
    </div>
  );
}

export function MailList() {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const listTab = useUIStore((s) => s.listTab);
  const mutedMessageIds = useUIStore((s) => s.mutedMessageIds);
  const token = useAuthStore((s) => s.token);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const removeMessage = useMailStore((s) => s.removeMessage);
  const replaceMessagesForView = useMailStore((s) => s.replaceMessagesForView);
  const accounts = useMailStore((s) => s.accounts);
  const messages = useMailStore((s) => s.messages);
  const folders = useMailStore((s) => s.folders);
  const getMessagesForView = useMailStore((s) => s.getMessagesForView);
  const showAccountBadge = selectedAccountId === ALL_ACCOUNTS;
  const items = useMemo(
    () =>
      getMessagesForView(
        {
          accountId: selectedAccountId,
          folderId: selectedFolderId,
          folderRole: selectedFolderRole,
        },
        { messages, folders },
      ),
    [
      getMessagesForView,
      messages,
      folders,
      selectedAccountId,
      selectedFolderId,
      selectedFolderRole,
    ],
  );

  const [loading, setLoading] = useState(false);
  const [searchHits, setSearchHits] = useState<MailMessage[] | null>(null);
  const [fetchError, setFetchError] = useState<{
    message: string;
    variant: ErrorBannerVariant;
  } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const resolveFetchError = useCallback(
    (err: unknown): { message: string; variant: ErrorBannerVariant } => {
      if (err instanceof ApiError && err.code === 'network') {
        return { message: t(locale, 'common.offline'), variant: 'offline' };
      }
      if (err instanceof ApiError) {
        return { message: err.message, variant: 'error' };
      }
      return { message: t(locale, 'common.loadError'), variant: 'error' };
    },
    [locale],
  );

  const viewOpts = useMemo(
    () => ({
      accountId: selectedAccountId,
      folderId: selectedFolderId,
      folderRole: selectedFolderRole,
    }),
    [selectedAccountId, selectedFolderId, selectedFolderRole],
  );

  const loadMessages = useCallback(async () => {
    if (!token) return;
    setLoading(true);
    try {
      const mapped = await fetchMessagesForView(viewOpts);
      replaceMessagesForView(viewOpts, mapped);
      setFetchError(null);
    } catch (err) {
      setFetchError(resolveFetchError(err));
    } finally {
      setLoading(false);
    }
  }, [token, viewOpts, replaceMessagesForView, resolveFetchError]);

  useEffect(() => {
    if (!token) return;

    const q = searchQuery.trim();
    if (q.length >= 2) {
      const handle = window.setTimeout(() => {
        void (async () => {
          try {
            const params = new URLSearchParams({ q });
            if (selectedAccountId !== ALL_ACCOUNTS) params.set('accountId', selectedAccountId);
            if (selectedFolderId) params.set('folderId', selectedFolderId);
            const data = await api<ApiMessage[]>(`/messages/search?${params}`);
            const mapped = data.map(mapApiMessage);
            for (const msg of mapped) upsertMessage(msg);
            setSearchHits(mapped);
            setFetchError(null);
          } catch (err) {
            setSearchHits([]);
            setFetchError(resolveFetchError(err));
          }
        })();
      }, 280);
      return () => window.clearTimeout(handle);
    }

    // Folder/role view reload — a fetch effect synchronizing with the
    // server; loadMessages shows its loading state synchronously on purpose.
    // oxlint-disable-next-line set-state-in-effect
    void loadMessages();
  }, [
    token,
    searchQuery,
    selectedAccountId,
    selectedFolderId,
    selectedFolderRole,
    upsertMessage,
    loadMessages,
    resolveFetchError,
  ]);

  useEffect(() => {
    if (!token || searchQuery.trim().length >= 2) return;
    const sub = syncEvents$.subscribe((ev) => {
      if (ev.type !== 'sync_complete') return;
      if (selectedAccountId !== ALL_ACCOUNTS && ev.accountId !== selectedAccountId) return;
      void loadMessages();
    });
    return () => sub.unsubscribe();
  }, [token, searchQuery, selectedAccountId, loadMessages]);

  // Search hits only apply while a query is active — masking the stored
  // hits during render keeps the exit-from-search path effect-free.
  const searching = Boolean(token) && searchQuery.trim().length >= 2;
  const activeHits = searching ? searchHits : null;
  const source = activeHits ?? items;
  const filtered = (listTab === 'unread' ? source.filter((item) => !item.isRead) : source).filter(
    (item) => !mutedMessageIds.includes(item.id),
  );
  // One row per conversation; the latest message drives the row.
  const conversations = useMemo(() => groupIntoConversations(filtered), [filtered]);

  // Interleave sticky day-group headers (Today / Yesterday / This week …).
  const listRows = useMemo(() => {
    type Row =
      { type: 'header'; key: GroupKey } | { type: 'convo'; convo: (typeof conversations)[number] };
    const rows: Row[] = [];
    let lastKey: GroupKey | '' = '';
    for (const convo of conversations) {
      const k = dayGroupKey(convo.latest.date);
      if (k !== lastKey) {
        rows.push({ type: 'header', key: k });
        lastKey = k;
      }
      rows.push({ type: 'convo', convo });
    }
    return rows;
  }, [conversations]);

  if (loading && filtered.length === 0 && !fetchError) {
    return (
      <div className="p-8 text-center text-muted-foreground">{t(locale, 'common.loading')}</div>
    );
  }

  if (filtered.length === 0 && !fetchError) {
    const isSearch = activeHits !== null;
    return (
      <EmptyState
        icon={isSearch ? SearchX : Inbox}
        title={t(locale, 'mail.noMessages')}
        hint={isSearch ? undefined : t(locale, 'mail.noMessagesHint')}
      />
    );
  }

  return (
    <div className="flex h-full flex-col">
      {fetchError ? (
        <ErrorBanner
          message={fetchError.message}
          variant={fetchError.variant}
          retryLabel={t(locale, 'common.retry')}
          onRetry={() => {
            if (searchQuery.trim().length >= 2) {
              setSearchHits(null);
            } else {
              void loadMessages();
            }
          }}
        />
      ) : null}
      {actionError ? (
        <div className="border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          {actionError}
        </div>
      ) : null}
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col">
          {listRows.map((row) => {
            if (row.type === 'header') {
              return (
                <div
                  key={`h-${row.key}`}
                  className="sticky top-0 z-10 -mx-0 bg-background/85 px-4 pt-3 pb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground backdrop-blur-sm"
                >
                  {t(locale, `mail.${row.key}`)}
                </div>
              );
            }
            const convo = row.convo;
            const item = convo.latest;
            const account = accounts.find((a) => a.id === item.accountId);
            const accountLabel = account?.displayName || account?.emailAddress;
            const labels = messageLabels(item);
            const fromLabel = item.from.name ?? item.from.email;
            const isSelected = convo.messages.some((m) => m.id === selectedMessageId);
            const isUnread = convo.unreadCount > 0;
            let relative = '';
            try {
              relative = formatDistanceToNow(new Date(item.date), {
                addSuffix: true,
                locale: locale === 'zh' ? zhCN : undefined,
              });
            } catch {
              relative = item.date;
            }
            const snippet = (item.snippet || item.bodyText || '').replace(/\s+/g, ' ').trim();
            const subjectNorm = (item.subject || '').replace(/\s+/g, ' ').trim();
            const showSnippet =
              snippet.length > 0 &&
              (subjectNorm.length === 0 || !snippet.startsWith(subjectNorm.slice(0, 60)));
            const hasAttachments = (item.attachments ?? []).some((a) => !a.isInline);
            const quickAction = (e: React.MouseEvent, action: 'archive' | 'trash') => {
              e.stopPropagation();
              void api(`/messages/${item.id}/${action}`, { method: 'POST' })
                .then(() => {
                  removeMessage(item.id);
                  if (selectedMessageId === item.id) setSelectedMessage(null);
                })
                .catch(() => {});
            };
            return (
              <DraggableConversationRow key={convo.key} convo={convo}>
                <ConversationContextMenu convo={convo} onActionError={setActionError}>
                  <div
                    role="button"
                    tabIndex={0}
                    className={cn(
                      'group relative flex w-full cursor-pointer gap-3 border-b border-border/60 px-4 py-3 text-left text-sm transition-[background-color,box-shadow] duration-150 ease-out-quart hover:bg-accent/40',
                      isSelected &&
                        'bg-secondary shadow-[inset_2px_0_0_var(--color-foreground)] hover:bg-secondary',
                    )}
                    onClick={() => {
                      const target = convo.messages.find((m) => !m.isRead) ?? convo.latest;
                      setSelectedMessage(target.id);
                    }}
                    onContextMenu={() => {
                      const target = convo.messages.find((m) => !m.isRead) ?? convo.latest;
                      setSelectedMessage(target.id);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        const target = convo.messages.find((m) => !m.isRead) ?? convo.latest;
                        setSelectedMessage(target.id);
                      }
                    }}
                  >
                    <div className="absolute right-3 top-2 hidden items-center gap-0.5 rounded-[7px] border border-input bg-card p-0.5 shadow-whisper group-hover:flex">
                      <button
                        type="button"
                        className="flex size-6 items-center justify-center rounded-[5px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                        title={t(locale, 'mail.archive')}
                        aria-label={t(locale, 'mail.archive')}
                        onClick={(e) => quickAction(e, 'archive')}
                      >
                        <Archive className="size-3.5" aria-hidden />
                      </button>
                      <button
                        type="button"
                        className="flex size-6 items-center justify-center rounded-[5px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                        title={t(locale, 'mail.moveToTrash')}
                        aria-label={t(locale, 'mail.moveToTrash')}
                        onClick={(e) => quickAction(e, 'trash')}
                      >
                        <Trash2 className="size-3.5" aria-hidden />
                      </button>
                    </div>
                    <div className="flex w-3 shrink-0 items-start justify-center pt-1.5">
                      {convo.anyReplied ? (
                        <CornerUpLeft
                          className={cn('h-3 w-3', 'text-ter-foreground')}
                          aria-hidden
                        />
                      ) : isUnread ? (
                        <span className="size-1.5 rounded-full bg-unread" aria-hidden />
                      ) : null}
                    </div>
                    <ListAvatar email={item.from.email} label={fromLabel} />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <div className={cn('min-w-0 truncate', isUnread && 'font-semibold')}>
                          {fromLabel}
                        </div>
                        {convo.messages.length > 1 ? (
                          <span
                            className="shrink-0 rounded-full border border-border px-1.5 text-[11px] leading-4 tabular-nums text-muted-foreground"
                            aria-label={t(locale, 'mail.conversationCount', {
                              count: convo.messages.length,
                            })}
                          >
                            {convo.messages.length}
                          </span>
                        ) : null}
                        {showAccountBadge && accountLabel ? (
                          <Badge
                            variant="outline"
                            className="max-w-[8rem] shrink-0 truncate rounded-md px-1.5 py-0 text-[11px] font-normal"
                          >
                            {accountLabel}
                          </Badge>
                        ) : null}
                        <div
                          className={cn(
                            'ml-auto shrink-0 text-[11px] tabular-nums text-muted-foreground transition-opacity group-hover:opacity-0',
                          )}
                        >
                          {relative}
                        </div>
                      </div>
                      <div
                        className={cn(
                          'mt-0.5 truncate text-[13px] leading-snug',
                          isUnread ? 'font-medium text-foreground' : 'text-foreground/90',
                        )}
                      >
                        {item.subject || '—'}
                      </div>
                      {showSnippet ? (
                        <div className="mt-1 flex items-start gap-1 text-xs leading-relaxed text-muted-foreground">
                          {hasAttachments ? (
                            <Paperclip
                              className="mt-0.5 size-3 shrink-0 text-ter-foreground"
                              aria-hidden
                            />
                          ) : null}
                          <span className="line-clamp-2 min-w-0">{snippet.slice(0, 300)}</span>
                        </div>
                      ) : null}
                      {labels.length ? (
                        <div className="mt-2 flex flex-wrap items-center gap-1.5">
                          {labels.map((label) => (
                            <Badge
                              key={label}
                              variant={getBadgeVariantFromLabel(label)}
                              className="rounded-md"
                            >
                              {label}
                            </Badge>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  </div>
                </ConversationContextMenu>
              </DraggableConversationRow>
            );
          })}
        </div>
      </ScrollArea>
    </div>
  );
}
