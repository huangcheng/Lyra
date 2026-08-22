/**
 * Message list — shadcn v3 mail example (All / Unread tabs live in the parent).
 */

import type { ComponentProps } from 'react';
import { formatDistanceToNow } from 'date-fns';
import { useEffect, useMemo, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { t } from '@/i18n';
import { ALL_ACCOUNTS, mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { cn } from '@/lib/utils';
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
            const res = await fetch(`/api/v1/messages/search?${params}`, {
              headers: { Authorization: `Bearer ${token}` },
            });
            if (!res.ok) throw new Error('Search failed');
            const data = (await res.json()) as ApiMessage[];
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

    const load = async () => {
      setLoading(true);
      try {
        let url: string | null = null;
        if (selectedFolderId) {
          url = `/api/v1/folders/${selectedFolderId}/messages`;
        } else if (selectedFolderRole) {
          const params = new URLSearchParams({ role: selectedFolderRole });
          if (selectedAccountId !== ALL_ACCOUNTS) params.set('accountId', selectedAccountId);
          url = `/api/v1/messages?${params}`;
        }
        if (!url) return;
        const res = await fetch(url, { headers: { Authorization: `Bearer ${token}` } });
        if (!res.ok) throw new Error('Failed to fetch messages');
        const data = (await res.json()) as ApiMessage[];
        for (const msg of data) upsertMessage(mapApiMessage(msg));
      } catch {
        /* keep existing */
      } finally {
        setLoading(false);
      }
    };

    void load();
  }, [token, searchQuery, selectedAccountId, selectedFolderId, selectedFolderRole, upsertMessage]);

  const source = searchHits ?? items;
  const filtered = listTab === 'unread' ? source.filter((item) => !item.isRead) : source;

  if (loading && filtered.length === 0) {
    return (
      <div className="p-8 text-center text-muted-foreground">{t(locale, 'common.loading')}</div>
    );
  }

  if (filtered.length === 0) {
    return (
      <div className="p-8 text-center text-muted-foreground">{t(locale, 'mail.noMessages')}</div>
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
                'flex flex-col items-start gap-2 rounded-lg border p-3 text-left text-sm transition-all hover:bg-accent',
                selectedMessageId === item.id && 'bg-muted',
              )}
              onClick={() => setSelectedMessage(item.id)}
            >
              <div className="flex w-full flex-col gap-1">
                <div className="flex items-center">
                  <div className="flex items-center gap-2">
                    <div className="font-semibold">{item.from.name ?? item.from.email}</div>
                    {!item.isRead ? (
                      <span className="flex h-2 w-2 rounded-full bg-blue-600" />
                    ) : null}
                  </div>
                  <div
                    className={cn(
                      'ml-auto text-xs',
                      selectedMessageId === item.id ? 'text-foreground' : 'text-muted-foreground',
                    )}
                  >
                    {relative}
                  </div>
                </div>
                <div className="text-xs font-medium">{item.subject}</div>
              </div>
              <div className="line-clamp-2 text-xs text-muted-foreground">
                {(item.snippet || item.bodyText || '').slice(0, 300)}
              </div>
              {labels.length ? (
                <div className="flex items-center gap-2">
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
            </button>
          );
        })}
      </div>
    </ScrollArea>
  );
}
