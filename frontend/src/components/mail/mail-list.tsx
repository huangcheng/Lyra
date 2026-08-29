/**
 * Message list — shadcn v3 mail example (All / Unread tabs live in the parent).
 */

import type { ComponentProps } from 'react';
import { formatDistanceToNow } from 'date-fns';
import { CornerUpLeft, Inbox, SearchX } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { EmptyState } from '@/components/empty-state';
import { ErrorBanner, type ErrorBannerVariant } from '@/components/error-banner';
import { Avatar, AvatarFallback } from '@/components/ui/avatar';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { t } from '@/i18n';
import { ApiError, api } from '@/lib/api-client';
import { groupIntoConversations } from '@/lib/conversation';
import { fetchMessagesForView } from '@/lib/load-mail-messages';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { getInitials, cn } from '@/lib/utils';
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

function getBadgeVariantFromLabel(label: string): ComponentProps<typeof Badge>['variant'] {
  if (label.toLowerCase() === 'work' || label.toLowerCase() === 'important') {
    return 'default';
  }
  if (label.toLowerCase() === 'personal') {
    return 'outline';
  }
  return 'secondary';
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
  const replaceMessagesForView = useMailStore((s) => s.replaceMessagesForView);
  const accounts = useMailStore((s) => s.accounts);
  const messages = useMailStore((s) => s.messages);
  const folders = useMailStore((s) => s.folders);
  const getMessagesForView = useMailStore((s) => s.getMessagesForView);
  const showAccountBadge = selectedAccountId === ALL_ACCOUNTS;
  const items = useMemo(
    () =>
      getMessagesForView({
        accountId: selectedAccountId,
        folderId: selectedFolderId,
        folderRole: selectedFolderRole,
      }),
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

    setSearchHits(null);
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

  const source = searchHits ?? items;
  const filtered = (listTab === 'unread' ? source.filter((item) => !item.isRead) : source).filter(
    (item) => !mutedMessageIds.includes(item.id),
  );
  // One row per conversation; the latest message drives the row.
  const conversations = useMemo(() => groupIntoConversations(filtered), [filtered]);

  if (loading && filtered.length === 0 && !fetchError) {
    return (
      <div className="p-8 text-center text-muted-foreground">{t(locale, 'common.loading')}</div>
    );
  }

  if (filtered.length === 0 && !fetchError) {
    const isSearch = searchHits !== null && searchQuery.trim().length >= 2;
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
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-1.5 p-2">
          {conversations.map((convo) => {
            const item = convo.latest;
            const account = accounts.find((a) => a.id === item.accountId);
            const accountLabel = account?.displayName || account?.emailAddress;
            const labels = messageLabels(item);
            const fromLabel = item.from.name ?? item.from.email;
            const isSelected = convo.messages.some((m) => m.id === selectedMessageId);
            const isUnread = convo.unreadCount > 0;
            let relative = '';
            try {
              relative = formatDistanceToNow(new Date(item.date), { addSuffix: true });
            } catch {
              relative = item.date;
            }
            const snippet = (item.snippet || item.bodyText || '').replace(/\s+/g, ' ').trim();
            const subjectNorm = (item.subject || '').replace(/\s+/g, ' ').trim();
            const showSnippet =
              snippet.length > 0 &&
              (subjectNorm.length === 0 || !snippet.startsWith(subjectNorm.slice(0, 60)));
            return (
              <button
                key={convo.key}
                type="button"
                className={cn(
                  'flex w-full gap-3 rounded-lg border border-border/70 bg-card px-3 py-2.5 text-left text-sm transition-colors hover:border-border',
                  isSelected && 'border-input shadow-whisper hover:border-input',
                )}
                onClick={() => {
                  const target = convo.messages.find((m) => !m.isRead) ?? convo.latest;
                  setSelectedMessage(target.id);
                }}
              >
                <div className="flex w-3 shrink-0 items-start justify-center pt-1.5">
                  {convo.anyReplied ? (
                    <CornerUpLeft
                      className={cn(
                        'h-3 w-3',
                        isSelected ? 'text-muted-foreground' : 'text-ter-foreground',
                      )}
                      aria-hidden
                    />
                  ) : isUnread ? (
                    <span className="size-1.5 rounded-full bg-unread" aria-hidden />
                  ) : null}
                </div>
                <Avatar className="h-8 w-8 shrink-0">
                  <AvatarFallback className="bg-muted text-xs text-foreground">
                    {getInitials(fromLabel)}
                  </AvatarFallback>
                </Avatar>
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
                        'ml-auto shrink-0 text-[11px] tabular-nums',
                        isSelected ? 'text-muted-foreground' : 'text-ter-foreground',
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
                    <div className="mt-1 line-clamp-2 text-xs leading-relaxed text-muted-foreground">
                      {snippet.slice(0, 300)}
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
              </button>
            );
          })}
        </div>
      </ScrollArea>
    </div>
  );
}
