import { describe, expect, it } from 'vitest';

import { useUIStore } from '@/stores/ui';

describe('accountOrder', () => {
  it('defaults to empty and setAccountOrder replaces it', () => {
    expect(useUIStore.getState().accountOrder).toEqual([]);
    useUIStore.getState().setAccountOrder(['b', 'a']);
    expect(useUIStore.getState().accountOrder).toEqual(['b', 'a']);
    useUIStore.getState().setAccountOrder([]);
  });
});
