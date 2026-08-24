import { Link } from '@tanstack/react-router';
import { ArrowLeft, type LucideIcon } from 'lucide-react';
import { StampLogo } from '@/components/stamp-logo';
import { cn } from '@/lib/utils';
import { t } from '@/i18n';
import { useUIStore } from '@/stores/ui';

export type SlimNavItem = {
  key: string;
  label: string;
  icon: LucideIcon;
  active?: boolean;
  onClick?: () => void;
};

export function SlimPageNav({ section, items }: { section: string; items: SlimNavItem[] }) {
  const locale = useUIStore((s) => s.locale);
  return (
    <aside className="flex w-[220px] shrink-0 flex-col gap-px bg-secondary px-2 py-3">
      <div className="flex items-center gap-2.5 px-2.5 pb-3 pt-1">
        <StampLogo size={28} />
        <span className="font-brand text-lg text-foreground">Lyra</span>
      </div>
      <Link
        to="/"
        className="mb-1 flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-[13px] text-muted-foreground hover:bg-accent"
      >
        <ArrowLeft size={16} /> {t(locale, 'nav.mail')}
      </Link>
      <div className="px-2.5 pb-1 pt-0.5 text-[10.5px] font-semibold uppercase tracking-[0.8px] text-ter-foreground">
        {section}
      </div>
      {items.map((item) => (
        <button
          key={item.key}
          onClick={item.onClick}
          className={cn(
            'flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[13px]',
            item.active
              ? 'bg-accent font-medium text-foreground'
              : 'text-foreground hover:bg-accent',
          )}
        >
          <item.icon
            size={16}
            className={item.active ? 'text-foreground' : 'text-ter-foreground'}
          />
          {item.label}
        </button>
      ))}
    </aside>
  );
}
