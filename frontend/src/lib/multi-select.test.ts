import { describe, expect, it } from 'vitest';

import {
  applyCmdShiftClick,
  applyShiftClick,
  EMPTY_SELECTION,
  extendSelection,
  isMultiSelection,
  rangeKeys,
  selectAll,
  singleSelect,
  targetMessageId,
  toggleKey,
} from '@/lib/multi-select';
import type { Conversation } from '@/lib/conversation';
import type { MailMessage } from '@/types';

const V = ['a', 'b', 'c', 'd', 'e'];

function msg(id: string, isRead = true): MailMessage {
  return {
    id,
    accountId: 'a1',
    folderId: 'f1',
    subject: id,
    from: { email: 'a@example.com' },
    to: [],
    date: '2026-09-01T10:00:00Z',
    snippet: '',
    isRead,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
  } as MailMessage;
}

function convo(messages: MailMessage[]): Conversation {
  return {
    key: 'th:x',
    messages,
    latest: messages[messages.length - 1],
    unreadCount: messages.filter((m) => !m.isRead).length,
    anyStarred: false,
    anyReplied: false,
  };
}

describe('singleSelect', () => {
  it('selects exactly one key as anchor and focus', () => {
    expect(singleSelect('b')).toEqual({ keys: ['b'], anchor: 'b', focus: 'b' });
  });
});

describe('toggleKey', () => {
  it('adds an unselected key and makes it anchor+focus', () => {
    expect(toggleKey(singleSelect('a'), 'c')).toEqual({
      keys: ['a', 'c'],
      anchor: 'c',
      focus: 'c',
    });
  });
  it('removes a selected non-anchor key, keeping the anchor', () => {
    const sel = { keys: ['a', 'c'], anchor: 'c', focus: 'c' };
    expect(toggleKey(sel, 'a')).toEqual({ keys: ['c'], anchor: 'c', focus: 'c' });
  });
  it('falls back to the last remaining key when the anchor is toggled off', () => {
    const sel = { keys: ['a', 'c'], anchor: 'c', focus: 'c' };
    expect(toggleKey(sel, 'c')).toEqual({ keys: ['a'], anchor: 'a', focus: 'a' });
  });
  it('empties the selection when the last key is toggled off', () => {
    expect(toggleKey(singleSelect('a'), 'a')).toEqual(EMPTY_SELECTION);
  });
});

describe('rangeKeys', () => {
  it('returns the inclusive slice in visible order, either direction', () => {
    expect(rangeKeys(V, 'b', 'd')).toEqual(['b', 'c', 'd']);
    expect(rangeKeys(V, 'd', 'b')).toEqual(['b', 'c', 'd']);
  });
  it('returns empty when either endpoint is not visible', () => {
    expect(rangeKeys(V, 'b', 'zzz')).toEqual([]);
    expect(rangeKeys([], 'a', 'b')).toEqual([]);
  });
});

describe('applyShiftClick', () => {
  it('replaces the selection with the anchor-to-target range', () => {
    const sel = { keys: ['a'], anchor: 'a', focus: 'a' };
    expect(applyShiftClick(sel, V, 'd')).toEqual({
      keys: ['a', 'b', 'c', 'd'],
      anchor: 'a',
      focus: 'd',
    });
  });
  it('single-selects when there is no anchor', () => {
    expect(applyShiftClick(EMPTY_SELECTION, V, 'c')).toEqual(singleSelect('c'));
  });
  it('single-selects when the anchor is no longer visible', () => {
    const sel = { keys: ['zzz'], anchor: 'zzz', focus: 'zzz' };
    expect(applyShiftClick(sel, V, 'c')).toEqual(singleSelect('c'));
  });
});

describe('applyCmdShiftClick', () => {
  it('unions the current selection with the anchor-to-target range', () => {
    const sel = { keys: ['e'], anchor: 'b', focus: 'b' };
    expect(applyCmdShiftClick(sel, V, 'd').keys).toEqual(['e', 'b', 'c', 'd']);
    expect(applyCmdShiftClick(sel, V, 'd').anchor).toBe('b');
  });
});

describe('extendSelection', () => {
  it('moves focus one row and keeps the anchor fixed', () => {
    const sel = { keys: ['b'], anchor: 'b', focus: 'b' };
    expect(extendSelection(V, sel, 1)).toEqual({
      keys: ['b', 'c'],
      anchor: 'b',
      focus: 'c',
    });
  });
  it('shrinks back toward the anchor', () => {
    const sel = { keys: ['b', 'c', 'd'], anchor: 'b', focus: 'd' };
    expect(extendSelection(V, sel, -1).keys).toEqual(['b', 'c']);
  });
  it('clamps at the list bounds', () => {
    const sel = { keys: ['a'], anchor: 'a', focus: 'a' };
    expect(extendSelection(V, sel, -1)).toEqual(sel);
    const last = { keys: ['e'], anchor: 'e', focus: 'e' };
    expect(extendSelection(V, last, 1)).toEqual(last);
  });
  it('starts from the edge when nothing is selected', () => {
    expect(extendSelection(V, EMPTY_SELECTION, 1)).toEqual(singleSelect('a'));
    expect(extendSelection(V, EMPTY_SELECTION, -1)).toEqual(singleSelect('e'));
  });
});

describe('selectAll', () => {
  it('selects every visible key, first as anchor and last as focus', () => {
    expect(selectAll(V)).toEqual({ keys: V, anchor: 'a', focus: 'e' });
    expect(selectAll([])).toEqual(EMPTY_SELECTION);
  });
});

describe('isMultiSelection', () => {
  it('is true only with more than one key', () => {
    expect(isMultiSelection(singleSelect('a'))).toBe(false);
    expect(isMultiSelection({ keys: ['a', 'b'], anchor: 'a', focus: 'b' })).toBe(true);
  });
});

describe('targetMessageId', () => {
  it('returns the first unread message even when it is not the latest', () => {
    const c = convo([msg('m1'), msg('m2', false), msg('m3')]);
    expect(c.latest.id).toBe('m3');
    expect(targetMessageId(c)).toBe('m2');
  });
  it('falls back to the latest message when all are read', () => {
    const c = convo([msg('m1'), msg('m2'), msg('m3')]);
    expect(targetMessageId(c)).toBe('m3');
  });
});
