/**
 * Apple Mail–style conversation multi-selection.
 *
 * Pure state transitions over conversation keys; the UI store
 * (`stores/ui.ts`) holds the current `{ keys, anchor, focus }` and the
 * components keep `selectedMessageId` synced to the anchor conversation's
 * target message. `visibleKeys` is always the current on-screen
 * conversation order (day-group header rows excluded).
 */

import type { Conversation } from '@/lib/conversation';

export interface ConversationSelection {
  /** Selected conversation keys, in click order. */
  keys: string[];
  /** Range anchor; also the reading pane's front card. */
  anchor: string | null;
  /** Moving edge for shift+↑/↓ extension. */
  focus: string | null;
}

export const EMPTY_SELECTION: ConversationSelection = { keys: [], anchor: null, focus: null };

export function isMultiSelection(sel: ConversationSelection): boolean {
  return sel.keys.length > 1;
}

export function singleSelect(key: string): ConversationSelection {
  return { keys: [key], anchor: key, focus: key };
}

/** Cmd/Ctrl+click: toggle one key. Toggling off the anchor falls back to the last remaining key. */
export function toggleKey(sel: ConversationSelection, key: string): ConversationSelection {
  if (sel.keys.includes(key)) {
    const keys = sel.keys.filter((k) => k !== key);
    const anchor = sel.anchor === key ? (keys[keys.length - 1] ?? null) : sel.anchor;
    const focus = sel.focus === key ? anchor : sel.focus;
    return { keys, anchor, focus };
  }
  return { keys: [...sel.keys, key], anchor: key, focus: key };
}

/** Inclusive slice of the visible order between two keys, either direction. */
export function rangeKeys(visibleKeys: string[], from: string, to: string): string[] {
  const a = visibleKeys.indexOf(from);
  const b = visibleKeys.indexOf(to);
  if (a === -1 || b === -1) return [];
  const [lo, hi] = a <= b ? [a, b] : [b, a];
  return visibleKeys.slice(lo, hi + 1);
}

/** Shift+click: selection becomes range(anchor…key); the anchor is unchanged. */
export function applyShiftClick(
  sel: ConversationSelection,
  visibleKeys: string[],
  key: string,
): ConversationSelection {
  if (!sel.anchor) return singleSelect(key);
  const range = rangeKeys(visibleKeys, sel.anchor, key);
  if (range.length === 0) return singleSelect(key);
  return { keys: range, anchor: sel.anchor, focus: key };
}

/** Cmd+Shift+click: union of the current selection and range(anchor…key). */
export function applyCmdShiftClick(
  sel: ConversationSelection,
  visibleKeys: string[],
  key: string,
): ConversationSelection {
  if (!sel.anchor) return singleSelect(key);
  const range = rangeKeys(visibleKeys, sel.anchor, key);
  if (range.length === 0) return toggleKey(sel, key);
  return { keys: [...new Set([...sel.keys, ...range])], anchor: sel.anchor, focus: key };
}

/** Shift+↑/↓: move the focus one row and select range(anchor…focus). */
export function extendSelection(
  visibleKeys: string[],
  sel: ConversationSelection,
  dir: 1 | -1,
): ConversationSelection {
  if (visibleKeys.length === 0) return sel;
  if (!sel.anchor || !sel.focus)
    return singleSelect(dir === 1 ? visibleKeys[0] : visibleKeys[visibleKeys.length - 1]);
  const idx = visibleKeys.indexOf(sel.focus);
  if (idx === -1) return singleSelect(sel.focus);
  const nextIdx = Math.min(Math.max(idx + dir, 0), visibleKeys.length - 1);
  if (nextIdx === idx) return sel;
  const focus = visibleKeys[nextIdx];
  return { keys: rangeKeys(visibleKeys, sel.anchor, focus), anchor: sel.anchor, focus };
}

export function selectAll(visibleKeys: string[]): ConversationSelection {
  if (visibleKeys.length === 0) return EMPTY_SELECTION;
  return {
    keys: [...visibleKeys],
    anchor: visibleKeys[0],
    focus: visibleKeys[visibleKeys.length - 1],
  };
}

/** The message a conversation opens to: first unread, else the latest. */
export function targetMessageId(convo: Conversation): string {
  return (convo.messages.find((m) => !m.isRead) ?? convo.latest).id;
}
