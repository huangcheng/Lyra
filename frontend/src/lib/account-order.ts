/**
 * Custom display order for sidebar accounts.
 *
 * `accountOrder` is the user-persisted id list (uiState blob); accounts not
 * listed (e.g. newly added) keep their server order at the end. Stale ids
 * (deleted accounts) never match and are effectively dropped.
 */

import type { MailAccount } from '@/types';

export function orderAccounts(accounts: MailAccount[], accountOrder: string[]): MailAccount[] {
  if (accountOrder.length === 0) return accounts;
  const rank = new Map(accountOrder.map((id, index) => [id, index]));
  // Array.prototype.sort is stable: unranked accounts keep server order.
  return [...accounts].sort((a, b) => {
    const ra = rank.get(a.id);
    const rb = rank.get(b.id);
    if (ra === undefined && rb === undefined) return 0;
    if (ra === undefined) return 1;
    if (rb === undefined) return -1;
    return ra - rb;
  });
}

/** Move `activeId` onto `overId`'s position within the rendered id order. */
export function moveId(ids: string[], activeId: string, overId: string): string[] {
  const from = ids.indexOf(activeId);
  const to = ids.indexOf(overId);
  if (from === -1 || to === -1 || from === to) return ids;
  const next = [...ids];
  next.splice(from, 1);
  next.splice(to, 0, activeId);
  return next;
}
