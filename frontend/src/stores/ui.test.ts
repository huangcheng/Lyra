import { describe, expect, it } from 'vitest';

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
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedAccount('acc-1');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
    useUIStore.getState().applyConversationSelection(singleSelect('a'), 'msg-1');
    useUIStore.getState().setSelectedFolderRole('inbox');
    expect(useUIStore.getState().selectedConversationKeys).toEqual([]);
  });
});
