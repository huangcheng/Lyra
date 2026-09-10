/**
 * Shared empty state: a perforated stamp frame around the icon (or the Lyra
 * stamp mark when no icon is given), a serif headline, and an optional hint.
 * The punched-hole edge is a dotted radial-gradient band covered by the inner
 * paper card — pure CSS, no assets.
 */

import type { LucideIcon } from 'lucide-react';

import { StampLogo } from '@/components/stamp-logo';
import { cn } from '@/lib/utils';

/** Punched perforation ring around the stamp paper. */
function StampFrame({ children, size }: { children: React.ReactNode; size: number }) {
  return (
    <div
      aria-hidden
      className="rounded-[4px] p-[7px]"
      style={{
        backgroundImage: 'radial-gradient(circle, var(--border) 1.3px, transparent 1.7px)',
        backgroundSize: '7px 7px',
      }}
    >
      <div
        className="flex items-center justify-center rounded-[2px] border border-border/60 bg-card"
        style={{ width: size, height: size }}
      >
        {children}
      </div>
    </div>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  hint,
  quiet = false,
}: {
  /** When omitted, the Lyra stamp mark takes the frame. */
  icon?: LucideIcon;
  title: string;
  hint?: string;
  /** Smaller frame, no icon disc — reader pane when nothing is selected. */
  quiet?: boolean;
}) {
  return (
    <div className="rise-in flex h-full min-h-[200px] flex-col items-center justify-center gap-3 p-8 text-center">
      <StampFrame size={quiet ? 72 : 64}>
        {Icon ? (
          <Icon
            className={cn('text-ter-foreground', quiet ? 'size-7' : 'size-6')}
            strokeWidth={1.5}
          />
        ) : (
          <StampLogo size={quiet ? 36 : 32} className="text-foreground" />
        )}
      </StampFrame>
      {/* CJK has no true italics — keep the serif voice, drop the slant. */}
      <p className="font-brand text-lg text-foreground italic [:lang(zh)]:not-italic">{title}</p>
      {hint ? <p className="max-w-xs text-sm text-muted-foreground">{hint}</p> : null}
    </div>
  );
}
