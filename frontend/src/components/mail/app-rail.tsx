/**
 * Far-left app rail (Fastmail/Yandex style): stamp + section icons.
 * Mail is the home view; contacts/calendar/dashboard/settings are routes.
 * Rendered only on the mail screen — standalone pages keep their slim nav.
 */

import { Link, useLocation } from '@tanstack/react-router';
import { BarChart3, CalendarDays, Mail, Settings, Users, type LucideIcon } from 'lucide-react';

import { StampLogo } from '@/components/stamp-logo';
import { ThemeToggle } from '@/components/theme-toggle';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { cn } from '@/lib/utils';
import { useUIStore } from '@/stores/ui';

interface RailItem {
  to: string;
  icon: LucideIcon;
  labelKey: string;
  /** Calendar shows today's day-of-month as a badge (Fastmail does this). */
  badge?: number;
}

export function AppRail() {
  const locale = useUIStore((s) => s.locale);
  const pathname = useLocation({ select: (s) => s.pathname });

  const top: RailItem[] = [
    { to: '/', icon: Mail, labelKey: 'nav.mail' },
    { to: '/contacts', icon: Users, labelKey: 'nav.contacts' },
    { to: '/calendar', icon: CalendarDays, labelKey: 'nav.calendar', badge: new Date().getDate() },
  ];
  const bottom: RailItem[] = [
    { to: '/dashboard', icon: BarChart3, labelKey: 'nav.dashboard' },
    { to: '/settings', icon: Settings, labelKey: 'nav.settings' },
  ];

  const railBtn = ({ to, icon: Icon, labelKey, badge }: RailItem) => {
    const active = pathname === to;
    return (
      <Tooltip key={to} delayDuration={0}>
        <TooltipTrigger asChild>
          <Link
            to={to}
            aria-label={t(locale, labelKey)}
            aria-current={active ? 'page' : undefined}
            className={cn(
              'relative flex size-9 items-center justify-center rounded-[9px] transition-colors',
              active
                ? 'bg-card text-foreground shadow-whisper'
                : 'text-ter-foreground hover:bg-accent/60 hover:text-foreground',
            )}
          >
            <Icon className="size-[17px]" />
            {badge ? (
              <span className="absolute -right-0.5 -top-0.5 min-w-3.5 rounded-full bg-unread px-0.5 text-center text-[9px] font-semibold leading-[14px] text-white">
                {badge}
              </span>
            ) : null}
          </Link>
        </TooltipTrigger>
        <TooltipContent side="right">{t(locale, labelKey)}</TooltipContent>
      </Tooltip>
    );
  };

  return (
    <nav
      aria-label={t(locale, 'nav.mail')}
      className="flex w-[52px] shrink-0 flex-col items-center gap-1 border-r border-border bg-sidebar py-2.5"
    >
      <Link to="/" className="mb-1.5" aria-label="Lyra">
        <StampLogo size={26} />
      </Link>
      {top.map(railBtn)}
      <div className="flex-1" />
      {bottom.map(railBtn)}
      <ThemeToggle isCollapsed />
    </nav>
  );
}
