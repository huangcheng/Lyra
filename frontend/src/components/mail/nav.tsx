/**
 * Sidebar folder / app links — shadcn v3 mail Nav.
 */

import { Link, useRouterState } from '@tanstack/react-router';
import type { LucideIcon } from 'lucide-react';

import { buttonVariants } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { cn } from '@/lib/utils';

export interface NavItem {
  title: string;
  label?: string;
  icon: LucideIcon;
  variant: 'default' | 'ghost';
  onClick?: () => void;
  href?: string;
}

export function Nav({ isCollapsed, links }: { isCollapsed: boolean; links: NavItem[] }) {
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  return (
    <div
      data-collapsed={isCollapsed}
      className="group flex flex-col gap-4 py-2 data-[collapsed=true]:py-2"
    >
      <nav className="grid gap-1 px-2 group-data-[collapsed=true]:justify-center group-data-[collapsed=true]:px-2">
        {links.map((link, index) => {
          const isActive = link.href ? pathname === link.href : link.variant === 'default';
          const variant = isActive ? 'default' : 'ghost';
          const className = cn(
            buttonVariants({ variant, size: isCollapsed ? 'icon' : 'sm' }),
            isCollapsed ? 'h-9 w-9' : 'h-9 w-full justify-start gap-2 px-3 has-[>svg]:px-3',
            variant === 'default' &&
              'dark:bg-muted dark:text-white dark:hover:bg-muted dark:hover:text-white',
          );

          const content = isCollapsed ? (
            <>
              <link.icon className="h-4 w-4" />
              <span className="sr-only">{link.title}</span>
            </>
          ) : (
            <>
              <link.icon className="h-4 w-4" />
              {link.title}
              {link.label ? (
                <span
                  className={cn(
                    'ml-auto font-normal tabular-nums',
                    variant === 'default' && 'text-background dark:text-white',
                  )}
                >
                  {link.label}
                </span>
              ) : null}
            </>
          );

          const inner = link.href ? (
            <Link key={index} to={link.href} className={className}>
              {content}
            </Link>
          ) : (
            <button key={index} type="button" className={className} onClick={link.onClick}>
              {content}
            </button>
          );

          if (isCollapsed) {
            return (
              <Tooltip key={index} delayDuration={0}>
                <TooltipTrigger asChild>{inner}</TooltipTrigger>
                <TooltipContent side="right" className="flex items-center gap-4">
                  {link.title}
                  {link.label ? (
                    <span className="ml-auto text-muted-foreground">{link.label}</span>
                  ) : null}
                </TooltipContent>
              </Tooltip>
            );
          }

          return inner;
        })}
      </nav>
    </div>
  );
}
