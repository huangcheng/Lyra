/**
 * Confirm before moving mail to trash (shared in-app dialog).
 */

import { confirmAction } from '@/lib/confirm-action';
import { t } from '@/i18n';
import type { SupportedLocale } from '@/types';

/** Returns true when the user accepts moving `count` message(s) to trash. */
export async function confirmMoveToTrash(locale: SupportedLocale, count = 1): Promise<boolean> {
  const title =
    count > 1 ? t(locale, 'mail.confirmTrashMany', { count }) : t(locale, 'mail.confirmTrash');
  return confirmAction({
    title,
    tone: 'destructive',
    confirmLabel: t(locale, 'common.confirm'),
    cancelLabel: t(locale, 'common.cancel'),
  });
}
