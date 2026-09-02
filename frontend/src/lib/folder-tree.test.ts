import { describe, expect, it } from 'vitest';

import { buildAccountMoveFolderEntries } from './folder-tree';
import type { MailFolder } from '@/types';

function folder(
  partial: Partial<MailFolder> & Pick<MailFolder, 'id' | 'name' | 'accountId'>,
): MailFolder {
  return {
    sortOrder: 0,
    totalCount: 0,
    unreadCount: 0,
    ...partial,
  };
}

describe('buildAccountMoveFolderEntries', () => {
  it('nests custom folders under parents instead of flattening alphabetically', () => {
    const accountId = 'acc1';
    const finance = folder({
      id: 'fin',
      name: '经济财政',
      accountId,
      sortOrder: 10,
    });
    const hk = folder({
      id: 'hk',
      name: '香港储蓄',
      accountId,
      parentId: 'fin',
      sortOrder: 1,
    });
    const bank = folder({
      id: 'bank',
      name: '工银亚洲',
      accountId,
      parentId: 'hk',
      sortOrder: 1,
    });
    const apple = folder({
      id: 'apple',
      name: 'Apple',
      accountId,
      sortOrder: 5,
    });
    const all = Object.fromEntries([finance, hk, bank, apple].map((f) => [f.id, f]));

    const rows = buildAccountMoveFolderEntries([finance, hk, bank, apple], all);

    expect(rows.map((r) => ({ name: r.name, depth: r.depth }))).toEqual([
      { name: 'Apple', depth: 0 },
      { name: '经济财政', depth: 0 },
      { name: '香港储蓄', depth: 1 },
      { name: '工银亚洲', depth: 2 },
    ]);
  });

  it('lists role folders first, with custom children nested under the role', () => {
    const accountId = 'acc1';
    const archive = folder({
      id: 'arch',
      name: 'Archive',
      accountId,
      role: 'archive',
      sortOrder: 0,
    });
    const nested = folder({
      id: 'nested',
      name: 'Receipts',
      accountId,
      parentId: 'arch',
      sortOrder: 0,
    });
    const custom = folder({
      id: 'custom',
      name: 'Projects',
      accountId,
      sortOrder: 1,
    });
    const all = Object.fromEntries([archive, nested, custom].map((f) => [f.id, f]));

    const rows = buildAccountMoveFolderEntries([archive, nested, custom], all);

    expect(rows.map((r) => ({ id: r.id, depth: r.depth, role: r.role ?? null }))).toEqual([
      { id: 'arch', depth: 0, role: 'archive' },
      { id: 'nested', depth: 1, role: null },
      { id: 'custom', depth: 0, role: null },
    ]);
  });
});
