/**
 * Manual "sync every account" button for the sidebar footer.
 * Loops the per-account trigger; the backend dedups queued/running jobs.
 * Spins while any account reports sync activity on the SSE stream.
 */

import { RefreshCw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { useSyncingAccounts } from '@/lib/use-syncing-accounts';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

export function SyncAllButton() {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const syncing = useSyncingAccounts().size > 0;
  const [failed, setFailed] = useState(false);

  const onClick = async () => {
    setFailed(false);
    const results = await Promise.all(
      accounts.map((a) =>
        api(`/accounts/${a.id}/sync`, { method: 'POST' }).then(
          () => true,
          () => false,
        ),
      ),
    );
    if (results.some((ok) => !ok)) setFailed(true);
  };

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8 shrink-0"
          disabled={syncing || accounts.length === 0}
          onClick={() => void onClick()}
          aria-label={t(locale, 'mail.syncAllAccounts')}
        >
          <RefreshCw
            className={cn('h-4 w-4', syncing && 'animate-spin', failed && 'text-destructive')}
          />
        </Button>
      </TooltipTrigger>
      <TooltipContent>
        {failed ? t(locale, 'mail.syncStartFailed') : t(locale, 'mail.syncAllAccounts')}
      </TooltipContent>
    </Tooltip>
  );
}
