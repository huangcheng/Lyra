import { afterEach, describe, expect, it } from 'vitest';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { singleSelect } from '@/lib/multi-select';
import { useUIStore } from '@/stores/ui';

describe('accountOrder', () => {
  it('defaults to empty and setAccountOrder replaces it', () => {
    expect(useUIStore.getState().accountOrder).toEqual([]);
    useUIStore.getState().setAccountOrder(['b', 'a']);
    expect(useUIStore.getState().accountOrder).toEqual(['b', 'a']);
    useUIStore.getState().setAccountOrder([]);
  });
});

describe('conversation multi-select', () => {
  // Reset everything the tests touch: account/folder/role back to defaults,
  // selection emptied, reader message cleared.
  afterEach(() => {
    useUIStore.getState().setSelectedAccount(ALL_ACCOUNTS);
  });

  it('applyConversationSelection sets keys, anchor, focus, and message together', () => {
    useUIStore
      .getState()
      .applyConversationSelection({ keys: ['a', 'b'], anchor: 'a', focus: 'b' }, 'msg-1');
    const s = useUIStore.getState();
    expect(s.selectedConversationKeys).toEqual(['a', 'b']);
    expect(s.selectionAnchorKey).toBe('a');
    expect(s.selectionFocusKey).toBe('b');
    expect(s.selectedMessageId).toBe('msg-1');
  });

  it('clearConversationSelection empties keys/anchor/focus but keeps the message', () => {
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().clearConversationSelection();
    const s = useUIStore.getState();
    expect(s.selectedConversationKeys).toEqual([]);
    expect(s.selectionAnchorKey).toBeNull();
    expect(s.selectionFocusKey).toBeNull();
    expect(s.selectedMessageId).toBe('msg-1');
  });

  it('folder/account switches clear the conversation selection', () => {
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedFolder('f1');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    expect(useUIStore.getState().selectionAnchorKey).toBeNull();
    expect(useUIStore.getState().selectionFocusKey).toBeNull();
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedAccount('acc-1');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    expect(useUIStore.getState().selectionAnchorKey).toBeNull();
    expect(useUIStore.getState().selectionFocusKey).toBeNull();
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedFolderRole('inbox');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    expect(useUIStore.getState().selectionAnchorKey).toBeNull();
    expect(useUIStore.getState().selectionFocusKey).toBeNull();
  });

  it('setSelectedMessage collapses the conversation selection', () => {
    useUIStore
      .getState()
      .applyConversationSelection({ keys: ['a', 'b'], anchor: 'a', focus: 'b' }, 'msg-1');
    useUIStore.getState().setSelectedMessage('x');
    const s = useUIStore.getState();
    expect(s.selectedConversationKeys).toEqual([]);
    expect(s.selectionAnchorKey).toBeNull();
    expect(s.selectionFocusKey).toBeNull();
    expect(s.selectedMessageId).toBe('x');
  });
});
