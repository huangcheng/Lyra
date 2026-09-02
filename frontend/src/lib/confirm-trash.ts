/**
 * Confirm before moving mail to trash.
 * Matches the native `confirm()` pattern used for account/key deletion in Settings.
 */

import { t } from '@/i18n';
import type { SupportedLocale } from '@/types';

/** Returns true when the user accepts moving `count` message(s) to trash. */
export function confirmMoveToTrash(locale: SupportedLocale, count = 1): boolean {
  const message =
    count > 1 ? t(locale, 'mail.confirmTrashMany', { count }) : t(locale, 'mail.confirmTrash');
  return window.confirm(message);
}
