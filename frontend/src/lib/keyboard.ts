/**
 * Global keyboard-shortcut decision logic.
 *
 * Pure functions so the ignore/trigger rules (never hijack typing, except
 * the explicit palette chord) are unit-testable without a DOM harness.
 */

export type GlobalShortcut =
  | 'palette' // ⌘K / Ctrl+K — works even while typing
  | 'palette-search' // / — open the palette in message-search mode
  | 'help' // ? — shortcut help
  | 'compose'; // c — new message

/** True when keystrokes belong to a text field, not to shortcuts. */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!target || !(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return Boolean(
    tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable,
  );
}

/**
 * Map a keydown to a global shortcut, or null when the event is not one.
 * `mod` = ⌘ on macOS, Ctrl elsewhere. Everything except `palette` is
 * suppressed while typing in an editable target.
 */
export function matchGlobalShortcut(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean },
  target: EventTarget | null,
  isMac = true,
): GlobalShortcut | null {
  const mod = isMac ? event.metaKey : event.ctrlKey;
  const other = isMac ? event.ctrlKey : event.metaKey;

  if (mod && !other && (event.key === 'k' || event.key === 'K')) {
    return 'palette';
  }
  if (isEditableTarget(target)) return null;

  switch (event.key) {
    case '/':
      return 'palette-search';
    case '?':
      return 'help';
    case 'c':
      return 'compose';
    default:
      return null;
  }
}

/**
 * Mail-list navigation decision: j/k move, o/Enter open, u/Escape back.
 * Returns null for anything else (or when typing).
 */
export type MailListShortcut = 'next' | 'prev' | 'open' | 'back' | null;

export function matchMailListShortcut(
  event: { key: string; shiftKey?: boolean },
  target: EventTarget | null,
): MailListShortcut {
  if (isEditableTarget(target) || event.shiftKey) return null;
  switch (event.key) {
    case 'j':
    case 'ArrowDown':
      return 'next';
    case 'k':
    case 'ArrowUp':
      return 'prev';
    case 'o':
    case 'Enter':
      return 'open';
    case 'u':
    case 'Escape':
      return 'back';
    default:
      return null;
  }
}

/**
 * Multi-select chords: ⌘A/Ctrl+A select-all, shift+↑/↓ (or shift+J/K)
 * extend. Returns null for anything else (or when typing).
 */
export type MailSelectionShortcut = 'select-all' | 'extend-next' | 'extend-prev' | null;

export function matchMailSelectionShortcut(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean },
  target: EventTarget | null,
  isMac = true,
): MailSelectionShortcut {
  if (isEditableTarget(target)) return null;
  const mod = isMac ? event.metaKey : event.ctrlKey;
  const other = isMac ? event.ctrlKey : event.metaKey;
  if (mod && !other && !event.shiftKey && (event.key === 'a' || event.key === 'A')) {
    return 'select-all';
  }
  if (event.shiftKey && !mod && !other) {
    if (event.key === 'ArrowDown' || event.key === 'J') return 'extend-next';
    if (event.key === 'ArrowUp' || event.key === 'K') return 'extend-prev';
  }
  return null;
}
