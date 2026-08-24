/**
 * Light / dark / system theme picker (dropdown).
 */

import { Monitor, Moon, Sun } from 'lucide-react';

import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import type { ThemeMode } from '@/lib/theme';
import { useUIStore } from '@/stores/ui';

export function ThemeToggle({ isCollapsed = false }: { isCollapsed?: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);

  const Icon = theme === 'dark' ? Moon : theme === 'light' ? Sun : Monitor;

  const items: { value: ThemeMode; label: string }[] = [
    { value: 'light', label: t(locale, 'settings.themeMode.light') },
    { value: 'dark', label: t(locale, 'settings.themeMode.dark') },
    { value: 'system', label: t(locale, 'settings.themeMode.system') },
  ];

  const trigger = (
    <Button
      variant="ghost"
      size="icon"
      className="h-8 w-8"
      aria-label={t(locale, 'settings.theme')}
    >
      <Icon className="h-4 w-4" />
    </Button>
  );

  return (
    <DropdownMenu>
      {isCollapsed ? (
        <Tooltip delayDuration={0}>
          <TooltipTrigger asChild>
            <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
          </TooltipTrigger>
          <TooltipContent side="right">{t(locale, 'settings.theme')}</TooltipContent>
        </Tooltip>
      ) : (
        <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      )}
      <DropdownMenuContent align="end">
        {items.map((item) => (
          <DropdownMenuItem key={item.value} onClick={() => setTheme(item.value)}>
            {item.label}
            {theme === item.value ? <span className="ml-auto text-primary">●</span> : null}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
