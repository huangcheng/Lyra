import { cn } from '@/lib/utils';

export function StampLogo({ size = 20, className }: { size?: number; className?: string }) {
  return (
    <span
      className={cn(
        'inline-flex items-center justify-center bg-primary font-brand text-primary-foreground',
        className,
      )}
      style={{
        width: size,
        height: size,
        borderRadius: Math.round(size * 0.22),
        fontSize: size * 0.6,
        lineHeight: 1,
      }}
      aria-hidden
    >
      L
    </span>
  );
}
