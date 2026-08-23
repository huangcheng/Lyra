/**
 * Mark-as-read policy helpers (values stored on `lyra_user.mark_read_policy`).
 */

import type { MarkReadPolicy } from '@/types';

export const MARK_READ_POLICIES: MarkReadPolicy[] = ['on_open', 'on_scroll_end', 'manual'];

/** Dwell before auto-mark when policy is `on_open` (accidental click guard). */
export const MARK_READ_OPEN_DWELL_MS = 2000;

export function isMarkReadPolicy(value: string): value is MarkReadPolicy {
  return MARK_READ_POLICIES.includes(value as MarkReadPolicy);
}
