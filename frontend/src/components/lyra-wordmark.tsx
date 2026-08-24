import { StampLogo } from '@/components/stamp-logo';
import { cn } from '@/lib/utils';

export function LyraWordmark({ className }: { className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-2', className)}>
      <StampLogo size={20} />
      <span className="font-brand text-[15px] leading-none text-foreground">Lyra</span>
    </span>
  );
}
