/**
 * Which account a compose is sent From. Resolution order (first hit that
 * still exists in `accounts` wins):
 *
 * 1. the draft's source account (reply/forward/edit-draft),
 * 2. the account being browsed (when not the unified view),
 * 3. the user's default account,
 * 4. the first account (legacy behavior).
 */

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import type { MailAccount } from '@/types';

export function resolveFromAccountId(opts: {
  draftAccountId?: string;
  selectedAccountId: string;
  defaultAccountId: string | null;
  accounts: MailAccount[];
}): string {
  const { draftAccountId, selectedAccountId, defaultAccountId, accounts } = opts;
  const exists = (id: string | null | undefined): id is string =>
    typeof id === 'string' && accounts.some((a) => a.id === id);
  if (exists(draftAccountId)) return draftAccountId;
  if (selectedAccountId !== ALL_ACCOUNTS && exists(selectedAccountId)) return selectedAccountId;
  if (exists(defaultAccountId)) return defaultAccountId;
  return accounts[0]?.id ?? '';
}
