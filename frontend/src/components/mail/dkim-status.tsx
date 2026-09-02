/**
 * DKIM status line + details popover for an expanded message card.
 * Renders nothing when the message was never verified (`dkim` null).
 */

import { ShieldAlert, ShieldCheck, ShieldMinus } from 'lucide-react';

import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { t } from '@/i18n';
import { dkimSummary } from '@/lib/dkim';
import { cn } from '@/lib/utils';
import type { DkimInfo, SupportedLocale } from '@/types';

function DetailRow({ label, value }: { label: string; value: string | null | undefined }) {
  if (!value) return null;
  return (
    <div className="flex gap-2 text-xs">
      <span className="w-36 shrink-0 text-muted-foreground">{label}</span>
      <span className="min-w-0 break-words">{value}</span>
    </div>
  );
}

export function DkimStatus({ dkim, locale }: { dkim: DkimInfo; locale: SupportedLocale }) {
  const summary = dkimSummary(locale, dkim);
  const Icon =
    dkim.status === 'pass' ? ShieldCheck : dkim.status === 'fail' ? ShieldAlert : ShieldMinus;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            'flex items-center gap-1.5 rounded-md px-1.5 py-1 text-xs transition-colors hover:bg-accent/60',
            dkim.status === 'pass' && 'text-green-700 dark:text-green-400',
            dkim.status === 'fail' && 'text-destructive',
            (dkim.status === 'none' || dkim.status === 'temperror') && 'text-muted-foreground',
          )}
        >
          <Icon className="size-3.5 shrink-0" aria-hidden />
          {summary}
        </button>
      </PopoverTrigger>
      <PopoverContent className="w-96">
        <div className="mb-2 text-sm font-medium">{t(locale, 'mail.dkimDetails')}</div>
        <div className="grid gap-1.5">
          <DetailRow label={t(locale, 'mail.dkimSdid')} value={dkim.sdid} />
          <DetailRow label={t(locale, 'mail.dkimAuid')} value={dkim.auid} />
          <DetailRow label={t(locale, 'mail.dkimSelector')} value={dkim.selector} />
          <DetailRow label={t(locale, 'mail.dkimAlgorithm')} value={dkim.algorithm} />
          <DetailRow
            label={t(locale, 'mail.dkimSignedHeaders')}
            value={dkim.signedHeaders.length ? dkim.signedHeaders.join(', ') : null}
          />
          <DetailRow
            label={t(locale, 'mail.dkimWarnings')}
            value={dkim.warnings.length ? dkim.warnings.join('; ') : null}
          />
          <DetailRow label={t(locale, 'mail.dkimSignedAt')} value={dkim.signedAt} />
          <DetailRow label={t(locale, 'mail.dkimExpiresAt')} value={dkim.expiresAt} />
        </div>
      </PopoverContent>
    </Popover>
  );
}
