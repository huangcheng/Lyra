/**
 * Page command registry for the command palette.
 *
 * One palette serves every page: global commands are always present and
 * each page (Calendar, Contacts, …) registers its own commands on mount.
 * Pure merge logic lives here so the dedupe/override rules are testable.
 */

import type { IconName } from 'react-cmdk';

export interface CommandDef {
  /** Stable id — a page command with the same id overrides a global one. */
  id: string;
  label: string;
  /** react-cmdk heroicons name shown as the leading icon. */
  icon?: IconName;
  keywords?: string[];
  /** Shortcut hint rendered on the right (e.g. `C`). */
  hint?: string;
  onSelect: () => void;
}

/**
 * Merge global commands with the active page's registrations.
 * Page commands win on id clashes (a page knows its context better) and
 * the order is: page commands first, then globals, deduped by id.
 */
export function mergeCommands(global: CommandDef[], page: CommandDef[]): CommandDef[] {
  const pageIds = new Set(page.map((c) => c.id));
  return [...page, ...global.filter((c) => !pageIds.has(c.id))];
}
