/**
 * Shared empty state: muted icon disc + headline + optional hint.
 */

import type { LucideIcon } from 'lucide-react';

import { cn } from '@/lib/utils';

export function EmptyState({
  icon: Icon,
  title,
  hint,
  quiet = false,
}: {
  icon: LucideIcon;
  title: string;
  hint?: string;
  /** Text-only — no icon disc (reader pane when nothing is selected). */
  quiet?: boolean;
}) {
  return (
    <div className="flex h-full min-h-[200px] flex-col items-center justify-center gap-2 p-8 text-center">
      {quiet ? null : (
        <div className="mb-1 flex h-12 w-12 items-center justify-center rounded-full bg-muted">
          <Icon className="h-6 w-6 text-muted-foreground" />
        </div>
      )}
      <p className={cn('text-sm', quiet ? 'font-normal text-muted-foreground' : 'font-medium')}>
        {title}
      </p>
      {hint ? <p className="max-w-xs text-sm text-muted-foreground">{hint}</p> : null}
    </div>
  );
}
