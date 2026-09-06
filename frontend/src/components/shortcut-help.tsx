/**
 * Shortcut help dialog (? anywhere, or from the palette).
 */

import { t } from '../i18n';
import { useUIStore } from '@/stores/ui';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';

export function ShortcutHelp() {
  const locale = useUIStore((s) => s.locale);
  const open = useUIStore((s) => s.shortcutHelpOpen);
  const setOpen = useUIStore((s) => s.setShortcutHelpOpen);

  const isMac =
    typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || '');
  const mod = isMac ? '⌘' : 'Ctrl';

  const rows: Array<[string, string]> = [
    [`${mod} K`, t(locale, 'palette.action.open')],
    ['/', t(locale, 'shortcuts.search')],
    ['C', t(locale, 'shortcuts.compose')],
    ['J / K', t(locale, 'shortcuts.nextPrev')],
    ['O / Enter', t(locale, 'shortcuts.open')],
    ['U / Esc', t(locale, 'shortcuts.back')],
    ['?', t(locale, 'shortcuts.help')],
  ];

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>{t(locale, 'shortcuts.title')}</DialogTitle>
          <DialogDescription>{t(locale, 'shortcuts.subtitle')}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-1">
          {rows.map(([keys, label]) => (
            <div key={keys} className="flex items-center justify-between gap-4 px-1 py-1">
              <span className="text-[13px] text-foreground">{label}</span>
              <kbd className="rounded border border-border bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                {keys}
              </kbd>
            </div>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
