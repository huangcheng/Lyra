/**
 * Lyra brand mark: four-point star + wordmark. Inline SVG, theme-aware.
 */

import { cn } from '@/lib/utils';

export function LyraWordmark({ className }: { className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-2 select-none', className)}>
      <svg
        viewBox="0 0 24 24"
        className="h-5 w-5 text-primary"
        fill="currentColor"
        aria-hidden="true"
      >
        <path d="M12 2c.6 4.8 4.6 8.8 9.4 9.4v1.2c-4.8.6-8.8 4.6-9.4 9.4h-1.2c-.6-4.8-4.6-8.8-9.4-9.4v-1.2C6.2 10.8 10.2 6.8 10.8 2H12Z" />
      </svg>
      <span className="text-lg font-semibold tracking-tight">Lyra</span>
    </span>
  );
}
