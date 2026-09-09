import { describe, expect, it } from 'vitest';

import {
  isEditableTarget,
  matchGlobalShortcut,
  matchMailListShortcut,
  matchMailSelectionShortcut,
} from './keyboard';

const INPUT = Object.assign(document.createElement('input'), {});
const BODY = document.createElement('div');

describe('isEditableTarget', () => {
  it('recognises inputs and contenteditable, ignores plain elements', () => {
    expect(isEditableTarget(INPUT)).toBe(true);
    expect(isEditableTarget(BODY)).toBe(false);
    const editable = document.createElement('div');
    Object.defineProperty(editable, 'isContentEditable', { value: true });
    expect(isEditableTarget(editable)).toBe(true);
    expect(isEditableTarget(null)).toBe(false);
  });
});

describe('matchGlobalShortcut', () => {
  it('⌘/Ctrl+K opens the palette even while typing', () => {
    expect(
      matchGlobalShortcut({ key: 'k', metaKey: true, ctrlKey: false, altKey: false }, INPUT, true),
    ).toBe('palette');
    expect(
      matchGlobalShortcut({ key: 'k', metaKey: false, ctrlKey: true, altKey: false }, BODY, false),
    ).toBe('palette');
    // the *other* modifier alone is not the palette
    expect(
      matchGlobalShortcut({ key: 'k', metaKey: false, ctrlKey: true, altKey: false }, BODY, true),
    ).toBeNull();
  });

  it('single keys trigger outside inputs and stay silent inside them', () => {
    expect(
      matchGlobalShortcut({ key: '/', metaKey: false, ctrlKey: false, altKey: false }, BODY, true),
    ).toBe('palette-search');
    expect(
      matchGlobalShortcut({ key: '?', metaKey: false, ctrlKey: false, altKey: false }, BODY, true),
    ).toBe('help');
    expect(
      matchGlobalShortcut({ key: 'c', metaKey: false, ctrlKey: false, altKey: false }, BODY, true),
    ).toBe('compose');
    expect(
      matchGlobalShortcut({ key: 'c', metaKey: false, ctrlKey: false, altKey: false }, INPUT, true),
    ).toBeNull();
    expect(
      matchGlobalShortcut({ key: 'x', metaKey: false, ctrlKey: false, altKey: false }, BODY, true),
    ).toBeNull();
  });
});

describe('matchMailListShortcut', () => {
  it('maps navigation keys and ignores the rest', () => {
    expect(matchMailListShortcut({ key: 'j' }, BODY)).toBe('next');
    expect(matchMailListShortcut({ key: 'ArrowUp' }, BODY)).toBe('prev');
    expect(matchMailListShortcut({ key: 'Enter' }, BODY)).toBe('open');
    expect(matchMailListShortcut({ key: 'Escape' }, BODY)).toBe('back');
    expect(matchMailListShortcut({ key: 'z' }, BODY)).toBeNull();
    expect(matchMailListShortcut({ key: 'j' }, INPUT)).toBeNull();
  });
});

describe('matchMailSelectionShortcut', () => {
  const noMod = { metaKey: false, ctrlKey: false };

  it('maps mod+A to select-all', () => {
    expect(
      matchMailSelectionShortcut({ key: 'a', ...noMod, metaKey: true, shiftKey: false }, null),
    ).toBe('select-all');
    expect(
      matchMailSelectionShortcut(
        { key: 'a', ...noMod, ctrlKey: true, shiftKey: false },
        null,
        false,
      ),
    ).toBe('select-all');
  });

  it('maps shift+arrows and shift+J/K to extend', () => {
    const ev = (key: string) => ({ key, ...noMod, shiftKey: true });
    expect(matchMailSelectionShortcut(ev('ArrowDown'), null)).toBe('extend-next');
    expect(matchMailSelectionShortcut(ev('ArrowUp'), null)).toBe('extend-prev');
    expect(matchMailSelectionShortcut(ev('J'), null)).toBe('extend-next');
    expect(matchMailSelectionShortcut(ev('K'), null)).toBe('extend-prev');
  });

  it('ignores plain keys, mod+shift+A, and editable targets', () => {
    expect(matchMailSelectionShortcut({ key: 'a', ...noMod, shiftKey: false }, null)).toBeNull();
    expect(
      matchMailSelectionShortcut({ key: 'a', metaKey: true, ctrlKey: false, shiftKey: true }, null),
    ).toBeNull();
    const input = document.createElement('input');
    expect(
      matchMailSelectionShortcut(
        { key: 'a', metaKey: true, ctrlKey: false, shiftKey: false },
        input,
      ),
    ).toBeNull();
  });

  it('plain navigation ignores shift (handled by the selection matcher)', () => {
    expect(matchMailListShortcut({ key: 'j', shiftKey: true }, null)).toBeNull();
    expect(matchMailListShortcut({ key: 'ArrowDown', shiftKey: true }, null)).toBeNull();
    expect(matchMailListShortcut({ key: 'j', shiftKey: false }, null)).toBe('next');
  });
});
