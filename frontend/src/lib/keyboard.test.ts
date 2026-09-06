import { describe, expect, it } from 'vitest';

import { isEditableTarget, matchGlobalShortcut, matchMailListShortcut } from './keyboard';

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
