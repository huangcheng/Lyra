/**
 * DKIM verdict display strings. The verdict comes from the detail payload;
 * `temperror` is shown as unsigned (it means "we couldn't check", not
 * "broken signature").
 */

import { t } from '@/i18n';
import type { DkimInfo, SupportedLocale } from '@/types';

export function dkimSummary(locale: SupportedLocale, dkim: DkimInfo): string {
  switch (dkim.status) {
    case 'pass':
      return dkim.sdid
        ? t(locale, 'mail.dkimValidSignedBy', { domain: dkim.sdid })
        : t(locale, 'mail.dkimValid');
    case 'fail':
      return t(locale, 'mail.dkimInvalid');
    default:
      return t(locale, 'mail.dkimNone');
  }
}
