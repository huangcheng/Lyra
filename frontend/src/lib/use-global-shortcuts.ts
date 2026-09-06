/**
 * Global keyboard shortcuts, mounted once in the root layout.
 *
 * ⌘K/Ctrl+K opens the palette (works while typing); / opens it seeded for
 * message search; ? opens the shortcut help; C composes. Decision rules
 * live in `lib/keyboard.ts` (unit-tested).
 */

import { useEffect } from 'react';

import { matchGlobalShortcut } from '@/lib/keyboard';
import { useUIStore } from '@/stores/ui';

export function useGlobalShortcuts(): void {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const isMac = /Mac|iPhone|iPad/.test(navigator.platform || '');
      const store = useUIStore.getState();
      switch (matchGlobalShortcut(e, e.target, isMac)) {
        case 'palette':
          e.preventDefault();
          store.setPaletteOpen(!store.paletteOpen);
          break;
        case 'palette-search':
          e.preventDefault();
          store.setPaletteOpen(true, '/');
          // the seed is a search hint, not literal text — start empty but
          // focused; drop the slash once open
          store.setPaletteSearch('');
          break;
        case 'help':
          e.preventDefault();
          store.setShortcutHelpOpen(true);
          break;
        case 'compose':
          e.preventDefault();
          store.openCompose();
          break;
        default:
          break;
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
