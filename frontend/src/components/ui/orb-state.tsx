import { ThinkingOrb } from 'thinking-orbs';
import type { OrbState as ThinkingOrbState } from 'thinking-orbs';
import { cn } from '@/lib/utils';

/**
 * Block-level loading state: a 64px thinking orb centered with an optional
 * label underneath. Use for full-pane loads (lists, pages, dialogs).
 */
export function OrbLoading({
  state = 'searching',
  label,
  className,
}: {
  state?: ThinkingOrbState;
  label?: string;
  className?: string;
}) {
  return (
    <div
      role="status"
      className={cn(
        'flex flex-col items-center justify-center gap-3 p-8 text-muted-foreground',
        className,
      )}
    >
      <ThinkingOrb state={state} size={64} />
      {label ? <span className="text-sm">{label}</span> : null}
    </div>
  );
}

/**
 * Inline loading indicator: a 20px thinking orb scaled to text size, with an
 * optional label. Use inside buttons, rows and muted "Loading…" text spots.
 */
export function InlineOrb({
  state = 'working',
  label,
  className,
}: {
  state?: ThinkingOrbState;
  label?: string;
  className?: string;
}) {
  return (
    <span role="status" className={cn('inline-flex items-center gap-1.5', className)}>
      <ThinkingOrb state={state} size={20} className="size-3.5 shrink-0" />
      {label ? <span>{label}</span> : null}
    </span>
  );
}
