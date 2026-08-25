/**
 * Inline error/offline banner for list and panel surfaces.
 */

import type { LucideIcon } from 'lucide-react';
import { AlertCircle, WifiOff } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

export type ErrorBannerVariant = 'error' | 'offline';

const variantMeta: Record<ErrorBannerVariant, { icon: LucideIcon; className: string }> = {
  error: {
    icon: AlertCircle,
    className: 'border-destructive/30 bg-destructive/10 text-destructive',
  },
  offline: {
    icon: WifiOff,
    className: 'border-muted-foreground/30 bg-muted text-muted-foreground',
  },
};

export function ErrorBanner({
  message,
  variant = 'error',
  retryLabel,
  onRetry,
  className,
}: {
  message: string;
  variant?: ErrorBannerVariant;
  retryLabel?: string;
  onRetry?: () => void;
  className?: string;
}) {
  const { icon: Icon, className: variantClass } = variantMeta[variant];

  return (
    <div
      className={cn('flex items-center gap-2 border-b px-4 py-2 text-sm', variantClass, className)}
      role="alert"
    >
      <Icon className="size-4 shrink-0" aria-hidden />
      <span className="min-w-0 flex-1">{message}</span>
      {onRetry ? (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 shrink-0 px-2 text-inherit hover:bg-black/5 dark:hover:bg-white/10"
          onClick={onRetry}
        >
          {retryLabel}
        </Button>
      ) : null}
    </div>
  );
}
