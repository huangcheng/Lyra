/**
 * Message list — shadcn v3 mail example (All / Unread tabs live in the parent).
 */

import type { ComponentProps } from 'react';
import { useDraggable } from '@dnd-kit/core';
import { formatDistanceToNow, isSameDay, isSameMonth, subDays } from 'date-fns';
import { zhCN } from 'date-fns/locale';
import { Archive, CornerUpLeft, Inbox, Paperclip, SearchX, Trash2 } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { matchMailListShortcut, matchMailSelectionShortcut } from '@/lib/keyboard';
import {
  applyCmdShiftClick,
  applyShiftClick,
  EMPTY_SELECTION,
  extendSelection,
  selectAll,
  singleSelect,
  targetMessageId,
  toggleKey,
  type ConversationSelection,
} from '@/lib/multi-select';

import { EmptyState } from '@/components/empty-state';
import { ErrorBanner, type ErrorBannerVariant } from '@/components/error-banner';
import { ConversationContextMenu } from '@/components/mail/conversation-context-menu';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { t } from '@/i18n';
import { ApiError, api } from '@/lib/api-client';
import { useAvatar } from '@/lib/avatar';
import { ThinkingOrb } from 'thinking-orbs';
import { confirmMoveToTrash } from '@/lib/confirm-trash';
import { groupIntoConversations, type Conversation } from '@/lib/conversation';
import type { ConversationDragData } from '@/lib/conversation-actions';
import { fetchMessagesForView } from '@/lib/load-mail-messages';
import { scheduleFolderRefresh } from '@/lib/refresh-folders';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { getInitials, avatarTone, cn } from '@/lib/utils';
import { useSyncingAccounts } from '@/lib/use-syncing-accounts';
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
  dragConvos,
  children,
}: {
  convo: Conversation;
  /** Non-null when dragging a row that is part of a multi-selection. */
  dragConvos?: Conversation[];
  children: React.ReactNode;
}) {
  // Cross-account moves are rejected per folder, so a mixed-account drag
  // only carries the dragged row's own account.
  const dragged = (dragConvos ?? [convo]).filter(
    (c) => c.latest.accountId === convo.latest.accountId,
  );
  const messageIds = dragged.flatMap((c) => c.messages.map((m) => m.id));
  const folderIds = [...new Set(dragged.flatMap((c) => c.messages.map((m) => m.folderId)))];
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
      count: dragged.length > 1 ? dragged.length : messageIds.length,
      selectionDrag: dragged.length > 1,
    } satisfies ConversationDragData,
  });
  return (
    <div ref={setNodeRef} {...listeners} className={cn(isDragging && 'opacity-40')}>
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
  const selectedConversationKeys = useUIStore((s) => s.selectedConversationKeys);
  const applyConversationSelection = useUIStore((s) => s.applyConversationSelection);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const listTab = useUIStore((s) => s.listTab);
  const mutedMessageIds = useUIStore((s) => s.mutedMessageIds);
  const token = useAuthStore((s) => s.token);
  const syncing = useSyncingAccounts().size > 0;
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
  const [searchLoading, setSearchLoading] = useState(false);
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
      // A server-restored selection that the freshly loaded view doesn't
      // contain (message moved/deleted, or read on the unread tab) dangles —
      // clear it so the reader doesn't wait on a message that won't come.
      const selected = useUIStore.getState().selectedMessageId;
      if (selected && !useMailStore.getState().messages[selected]) {
        useUIStore.getState().setSelectedMessage(null);
      }
      // Self-heal stale sidebar badges: empty list but folder still claims unread.
      if (mapped.length === 0 && viewOpts.folderId) {
        const folder = useMailStore.getState().folders[viewOpts.folderId];
        if (folder && folder.unreadCount > 0) scheduleFolderRefresh();
      } else if (mapped.length === 0 && viewOpts.folderRole) {
        const unified = useMailStore.getState().getUnifiedFolders();
        const row = unified.find((f) => f.role === viewOpts.folderRole);
        if (row && row.unreadCount > 0) scheduleFolderRefresh();
      }
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
          setSearchLoading(true);
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
          } finally {
            setSearchLoading(false);
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
  const showSearchOrb = searching && searchLoading;
  const activeHits = searching ? searchHits : null;
  const source = activeHits ?? items;
  const filtered = (listTab === 'unread' ? source.filter((item) => !item.isRead) : source).filter(
    (item) => !mutedMessageIds.includes(item.id),
  );
  // One row per conversation; the latest message drives the row.
  const conversations = useMemo(() => groupIntoConversations(filtered), [filtered]);
  const visibleKeys = useMemo(() => conversations.map((c) => c.key), [conversations]);
  const selectedConvos = useMemo(
    () => conversations.filter((c) => selectedConversationKeys.includes(c.key)),
    [conversations, selectedConversationKeys],
  );

  /** Current selection snapshot from the store (handlers read it lazily). */
  const currentSelection = (): ConversationSelection => {
    const s = useUIStore.getState();
    return {
      keys: s.selectedConversationKeys,
      anchor: s.selectionAnchorKey,
      focus: s.selectionFocusKey,
    };
  };

  /** Apply a new selection and point the reader at the anchor conversation. */
  const commitSelection = (sel: ConversationSelection) => {
    const anchorConvo = sel.anchor ? conversations.find((c) => c.key === sel.anchor) : undefined;
    applyConversationSelection(sel, anchorConvo ? targetMessageId(anchorConvo) : null);
  };

  // Gmail-style list navigation (j/k, o/Enter, u/Esc) plus multi-select
  // chords (⌘A, shift+↑/↓). Esc collapses an active multi-selection to its
  // anchor before falling back to the plain back behavior.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const isMac = /Mac|iPhone|iPad/.test(navigator.platform || '');
      const selAction = matchMailSelectionShortcut(e, e.target, isMac);
      if (selAction && conversations.length > 0) {
        e.preventDefault();
        if (selAction === 'select-all') {
          commitSelection(selectAll(visibleKeys));
        } else {
          commitSelection(
            extendSelection(visibleKeys, currentSelection(), selAction === 'extend-next' ? 1 : -1),
          );
        }
        return;
      }
      const action = matchMailListShortcut(e, e.target);
      if (!action || conversations.length === 0) return;
      const currentIdx = Math.max(
        0,
        conversations.findIndex((c) => c.messages.some((m) => m.id === selectedMessageId)),
      );
      if (action === 'next' || action === 'prev') {
        e.preventDefault();
        const idx =
          action === 'next'
            ? Math.min(currentIdx + 1, conversations.length - 1)
            : Math.max(currentIdx - 1, 0);
        commitSelection(singleSelect(conversations[idx].key));
      } else if (action === 'open') {
        const current = conversations[currentIdx];
        if (current && !current.messages.some((m) => m.id === selectedMessageId)) {
          e.preventDefault();
          commitSelection(singleSelect(current.key));
        }
      } else if (action === 'back') {
        const sel = currentSelection();
        if (sel.keys.length > 1 && sel.anchor) {
          e.preventDefault();
          commitSelection(singleSelect(sel.anchor));
        } else if (sel.keys.length === 1) {
          // Single selection made via the new key-based path: Esc clears
          // keys and reader together so highlight and j/k stay in sync.
          e.preventDefault();
          applyConversationSelection(EMPTY_SELECTION, null);
        } else if (selectedMessageId) {
          e.preventDefault();
          setSelectedMessage(null);
        }
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
    // oxlint-disable-next-line exhaustive-deps
  }, [conversations, visibleKeys, selectedMessageId, setSelectedMessage]);

  // Selection pruning: conversations that left the view (archived, moved,
  // tab/filter change) drop out of the selection; the anchor falls back to
  // the last surviving key.
  useEffect(() => {
    const s = useUIStore.getState();
    if (s.selectedConversationKeys.length === 0) return;
    const visible = new Set(visibleKeys);
    const keys = s.selectedConversationKeys.filter((k) => visible.has(k));
    if (keys.length === s.selectedConversationKeys.length) return;
    const anchor =
      s.selectionAnchorKey && visible.has(s.selectionAnchorKey)
        ? s.selectionAnchorKey
        : (keys[keys.length - 1] ?? null);
    const focus =
      s.selectionFocusKey && visible.has(s.selectionFocusKey) ? s.selectionFocusKey : anchor;
    const anchorConvo = anchor ? conversations.find((c) => c.key === anchor) : undefined;
    // oxlint-disable-next-line set-state-in-effect
    s.applyConversationSelection(
      { keys, anchor, focus },
      anchorConvo ? targetMessageId(anchorConvo) : null,
    );
  }, [conversations, visibleKeys]);

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
        title={
          !isSearch && syncing ? t(locale, 'mail.noMessagesSyncing') : t(locale, 'mail.noMessages')
        }
        hint={isSearch || syncing ? undefined : t(locale, 'mail.noMessagesHint')}
      />
    );
  }

  return (
    <div className="flex h-full flex-col">
      {showSearchOrb ? (
        <div className="flex items-center justify-center gap-2 py-4 text-muted-foreground">
          <ThinkingOrb state="searching" size={20} />
          <span className="text-xs">{t(locale, 'common.loading')}</span>
        </div>
      ) : null}
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
            const isSelected =
              selectedConversationKeys.length > 0
                ? selectedConversationKeys.includes(convo.key)
                : convo.messages.some((m) => m.id === selectedMessageId);
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
            const snippet = (item.snippet || '').replace(/\s+/g, ' ').trim();
            const subjectNorm = (item.subject || '').replace(/\s+/g, ' ').trim();
            const showSnippet =
              snippet.length > 0 &&
              (subjectNorm.length === 0 || !snippet.startsWith(subjectNorm.slice(0, 60)));
            const hasAttachments = (item.attachments ?? []).some((a) => !a.isInline);
            const quickAction = (e: React.MouseEvent, action: 'archive' | 'trash') => {
              e.stopPropagation();
              void (async () => {
                if (action === 'trash' && !(await confirmMoveToTrash(locale))) return;
                try {
                  await api(`/messages/${item.id}/${action}`, { method: 'POST' });
                  removeMessage(item.id);
                  if (selectedMessageId === item.id) setSelectedMessage(null);
                } catch {
                  /* list row quick actions stay quiet */
                }
              })();
            };
            return (
              <DraggableConversationRow
                key={convo.key}
                convo={convo}
                dragConvos={
                  selectedConversationKeys.length > 1 &&
                  selectedConversationKeys.includes(convo.key)
                    ? selectedConvos
                    : undefined
                }
              >
                <ConversationContextMenu
                  convo={convo}
                  multiConvos={
                    selectedConversationKeys.length > 1 &&
                    selectedConversationKeys.includes(convo.key)
                      ? selectedConvos
                      : undefined
                  }
                  onActionError={setActionError}
                >
                  <div
                    role="button"
                    tabIndex={0}
                    className={cn(
                      'group relative flex w-full cursor-pointer gap-3 border-b border-border/60 px-4 py-2 text-left text-sm transition-[background-color,box-shadow] duration-150 ease-out-quart select-none hover:bg-accent/40',
                      isSelected &&
                        'bg-secondary shadow-[inset_2px_0_0_var(--color-foreground)] hover:bg-secondary',
                    )}
                    onClick={(e) => {
                      const sel = currentSelection();
                      if (e.shiftKey && (e.metaKey || e.ctrlKey)) {
                        commitSelection(applyCmdShiftClick(sel, visibleKeys, convo.key));
                      } else if (e.shiftKey) {
                        commitSelection(applyShiftClick(sel, visibleKeys, convo.key));
                      } else if (e.metaKey || e.ctrlKey) {
                        commitSelection(toggleKey(sel, convo.key));
                      } else {
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
                    onContextMenu={() => {
                      // Apple Mail: right-click inside the selection keeps it;
                      // right-click elsewhere collapses the selection to that row.
                      if (!currentSelection().keys.includes(convo.key)) {
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
                    onKeyDown={(e) => {
                      if (
                        (e.key === 'Enter' || e.key === ' ') &&
                        !e.shiftKey &&
                        !e.metaKey &&
                        !e.ctrlKey
                      ) {
                        e.preventDefault();
                        commitSelection(singleSelect(convo.key));
                      }
                    }}
                  >
                    <div
                      role="toolbar"
                      aria-label={t(locale, 'mail.messageActions')}
                      className="absolute right-2 top-1/2 z-10 flex -translate-y-1/2 items-center gap-1 rounded-lg bg-background/95 p-1 opacity-0 shadow-none ring-1 ring-border/60 transition-opacity duration-100 pointer-events-none group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <button
                        type="button"
                        className="flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                        title={t(locale, 'mail.archive')}
                        aria-label={t(locale, 'mail.archive')}
                        onClick={(e) => quickAction(e, 'archive')}
                      >
                        <Archive className="size-4" aria-hidden />
                      </button>
                      <button
                        type="button"
                        className="flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                        title={t(locale, 'mail.moveToTrash')}
                        aria-label={t(locale, 'mail.moveToTrash')}
                        onClick={(e) => quickAction(e, 'trash')}
                      >
                        <Trash2 className="size-4" aria-hidden />
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
                        <div className="mt-0.5 flex items-center gap-1 text-xs leading-snug text-muted-foreground">
                          {hasAttachments ? (
                            <Paperclip
                              className="size-3 shrink-0 text-ter-foreground"
                              aria-hidden
                            />
                          ) : null}
                          <span className="min-w-0 truncate">{snippet.slice(0, 160)}</span>
                        </div>
                      ) : null}
                      {labels.length ? (
                        <div className="mt-1 flex flex-wrap items-center gap-1.5">
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
