/**
 * Sync status popover for the sidebar footer.
 * Per-account live status from the SSE stream: current folder + fetched/total
 * while syncing, the error text on failure, lastSyncAt when idle.
 * Each idle/errored account row has a "Sync now" action.
 */

import { formatDistanceToNow } from 'date-fns';
import { ChevronUp, Loader2, RefreshCw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { useSyncProgress } from '@/lib/sync-progress';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

export function SyncStatusPopover() {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const folders = useMailStore((s) => s.folders);
  const progress = useSyncProgress();
  const [requested, setRequested] = useState<ReadonlySet<string>>(new Set());
  const [failedIds, setFailedIds] = useState<ReadonlySet<string>>(new Set());

  const syncNow = async (accountId: string) => {
    setRequested((prev) => new Set(prev).add(accountId));
    setFailedIds((prev) => {
      const next = new Set(prev);
      next.delete(accountId);
      return next;
    });
    try {
      await api(`/accounts/${accountId}/sync`, { method: 'POST' });
    } catch {
      // Sync-start failures never emit an SSE sync_error, so flag them here.
      setFailedIds((prev) => new Set(prev).add(accountId));
    } finally {
      setRequested((prev) => {
        const next = new Set(prev);
        next.delete(accountId);
        return next;
      });
    }
  };

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-5 shrink-0"
          disabled={accounts.length === 0}
          aria-label={t(locale, 'sync.details')}
        >
          <ChevronUp className="h-3.5 w-3.5" />
        </Button>
      </PopoverTrigger>
      <PopoverContent side="top" align="end" className="w-80 p-1.5">
        {accounts.map((account) => {
          const status = progress.get(account.id);
          const folderName = status?.currentFolderId
            ? folders[status.currentFolderId]?.name
            : undefined;
          const lastSyncDate = account.lastSyncAt ? new Date(account.lastSyncAt) : undefined;
          const lastSyncValid = lastSyncDate !== undefined && !Number.isNaN(lastSyncDate.getTime());
          return (
            <div key={account.id} className="flex items-center gap-2 rounded-md px-2 py-1.5">
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] font-medium">
                  {account.displayName || account.emailAddress}
                </div>
                <div
                  className={cn(
                    'truncate text-[12px]',
                    status?.state === 'error' ? 'text-destructive' : 'text-muted-foreground',
                  )}
                  title={status?.state === 'error' ? (status.error ?? undefined) : undefined}
                >
                  {status?.state === 'syncing'
                    ? status.currentFolderId
                      ? t(locale, 'sync.syncingFolder', {
                          folder: folderName ?? t(locale, 'sync.folderFallback'),
                          fetched: status.fetched,
                          total: status.total,
                        })
                      : t(locale, 'sync.starting')
                    : status?.state === 'error'
                      ? (status.error ?? t(locale, 'sync.syncFailed'))
                      : lastSyncValid
                        ? `${t(locale, 'sync.lastSync')} ${formatDistanceToNow(lastSyncDate, { addSuffix: true })}`
                        : t(locale, 'sync.notSyncedYet')}
                </div>
              </div>
              {status?.state === 'syncing' ? (
                <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-muted-foreground" />
              ) : (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0"
                  disabled={requested.has(account.id)}
                  onClick={() => void syncNow(account.id)}
                  aria-label={t(locale, 'sync.syncAccount')}
                  title={failedIds.has(account.id) ? t(locale, 'mail.syncStartFailed') : undefined}
                >
                  <RefreshCw
                    className={cn(
                      'h-3.5 w-3.5',
                      requested.has(account.id) && 'animate-spin',
                      failedIds.has(account.id) && 'text-destructive',
                    )}
                  />
                </Button>
              )}
            </div>
          );
        })}
      </PopoverContent>
    </Popover>
  );
}
