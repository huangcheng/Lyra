/**
 * Message list — shadcn v3 mail example (All / Unread tabs live in the parent).
 */

import type { ComponentProps } from 'react';
import { formatDistanceToNow } from 'date-fns';
import { CornerUpLeft, Inbox, SearchX } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { EmptyState } from '@/components/empty-state';
import { Avatar, AvatarFallback } from '@/components/ui/avatar';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { fetchMessagesForView } from '@/lib/load-mail-messages';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { getInitials, cn } from '@/lib/utils';
import { syncEvents$ } from '@/rxjs/sync-events';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

function messageLabels(item: MailMessage, accountLabel?: string): string[] {
  const labels: string[] = [];
  if (accountLabel) labels.push(accountLabel);
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
  const token = useAuthStore((s) => s.token);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const replaceMessagesForView = useMailStore((s) => s.replaceMessagesForView);
  const accounts = useMailStore((s) => s.accounts);
  const messages = useMailStore((s) => s.messages);
  const folders = useMailStore((s) => s.folders);
  const getMessagesForView = useMailStore((s) => s.getMessagesForView);
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
    } catch {
      /* keep existing */
    } finally {
      setLoading(false);
    }
  }, [token, viewOpts, replaceMessagesForView]);

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
          } catch {
            setSearchHits([]);
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
  const filtered = listTab === 'unread' ? source.filter((item) => !item.isRead) : source;

  if (loading && filtered.length === 0) {
    return (
      <div className="p-8 text-center text-muted-foreground">{t(locale, 'common.loading')}</div>
    );
  }

  if (filtered.length === 0) {
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
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-2 p-4 pt-0">
        {filtered.map((item) => {
          const account = accounts.find((a) => a.id === item.accountId);
          const accountLabel =
            selectedAccountId === ALL_ACCOUNTS
              ? account?.displayName || account?.emailAddress
              : undefined;
          const labels = messageLabels(item, accountLabel);
          const fromLabel = item.from.name ?? item.from.email;
          const isSelected = selectedMessageId === item.id;
          let relative = '';
          try {
            relative = formatDistanceToNow(new Date(item.date), { addSuffix: true });
          } catch {
            relative = item.date;
          }
          return (
            <button
              key={item.id}
              type="button"
              className={cn(
                'flex w-full gap-3 rounded-lg border p-3 text-left text-sm transition-all hover:bg-accent/60',
                isSelected && 'border-border bg-card shadow-sm hover:bg-card',
                !item.isRead && !isSelected && 'bg-card/50',
              )}
              onClick={() => setSelectedMessage(item.id)}
            >
              <div className="flex w-3 shrink-0 items-start justify-center pt-1.5">
                {item.isReplied ? (
                  <CornerUpLeft className="h-3 w-3 text-muted-foreground" aria-hidden />
                ) : !item.isRead ? (
                  <span className="h-2 w-2 rounded-full bg-primary" aria-hidden />
                ) : null}
              </div>
              <Avatar className="h-8 w-8 shrink-0">
                <AvatarFallback className="bg-primary/10 text-xs text-primary">
                  {getInitials(fromLabel)}
                </AvatarFallback>
              </Avatar>
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline gap-2">
                  <div className={cn('truncate', !item.isRead && 'font-semibold')}>{fromLabel}</div>
                  <div
                    className={cn(
                      'ml-auto shrink-0 text-xs tabular-nums',
                      isSelected ? 'text-foreground' : 'text-muted-foreground',
                    )}
                  >
                    {relative}
                  </div>
                </div>
                <div
                  className={cn(
                    'truncate text-xs',
                    !item.isRead ? 'font-medium' : 'text-muted-foreground',
                  )}
                >
                  {item.subject}
                </div>
                <div className="mt-1 line-clamp-2 text-xs text-muted-foreground">
                  {(item.snippet || item.bodyText || '').slice(0, 300)}
                </div>
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
  );
}
