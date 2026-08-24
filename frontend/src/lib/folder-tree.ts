/**
 * Build a nested folder tree for the sidebar from flat folder rows.
 */

import type { MailFolder } from '@/types';

export interface FolderTreeNode {
  id: string;
  title: string;
  label?: string;
  children: FolderTreeNode[];
}

function sortFolders(a: MailFolder, b: MailFolder): number {
  return a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
}

/** Custom (non-role) folders as a tree for one account. */
export function buildCustomFolderTree(
  accountFolders: MailFolder[],
  allFolders: Record<string, MailFolder>,
): FolderTreeNode[] {
  const custom = accountFolders.filter((folder) => !folder.role);
  const customIds = new Set(custom.map((folder) => folder.id));

  const childrenOf = (parentId: string): MailFolder[] =>
    custom.filter((folder) => folder.parentId === parentId).sort(sortFolders);

  const toNode = (folder: MailFolder): FolderTreeNode => {
    const childFolders = childrenOf(folder.id);
    return {
      id: folder.id,
      title: folder.name,
      label: folder.unreadCount > 0 ? String(folder.unreadCount) : undefined,
      children: childFolders.map(toNode),
    };
  };

  const roots = custom
    .filter((folder) => {
      if (!folder.parentId) return true;
      const parent = allFolders[folder.parentId];
      if (!parent) return true;
      return Boolean(parent.role) || !customIds.has(folder.parentId);
    })
    .sort(sortFolders);

  return roots.map(toNode);
}
