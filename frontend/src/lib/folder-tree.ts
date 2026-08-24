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

/** Shared tree builder for one account's custom (non-role) folders. */
function makeTree(accountFolders: MailFolder[]) {
  const custom = accountFolders.filter((folder) => !folder.role);

  const childrenOf = (parentId: string): FolderTreeNode[] =>
    custom
      .filter((folder) => folder.parentId === parentId)
      .sort(sortFolders)
      .map((folder) => ({
        id: folder.id,
        title: folder.name,
        label: folder.unreadCount > 0 ? String(folder.unreadCount) : undefined,
        children: childrenOf(folder.id),
      }));

  return { custom, childrenOf };
}

/**
 * Custom (non-role) folders as a tree for one account.
 *
 * Roots are folders with no parent, a missing parent, or a parent that is
 * neither custom nor a role folder. Custom folders whose parent is a ROLE
 * folder (e.g. children of Archive) are excluded — they render nested under
 * that role row via `buildRoleChildren`.
 */
export function buildCustomFolderTree(
  accountFolders: MailFolder[],
  allFolders: Record<string, MailFolder>,
): FolderTreeNode[] {
  const { custom, childrenOf } = makeTree(accountFolders);
  const customIds = new Set(custom.map((folder) => folder.id));

  return custom
    .filter((folder) => {
      if (!folder.parentId) return true;
      const parent = allFolders[folder.parentId];
      if (!parent || parent.role) return false;
      return !customIds.has(folder.parentId);
    })
    .sort(sortFolders)
    .map((folder) => ({
      id: folder.id,
      title: folder.name,
      label: folder.unreadCount > 0 ? String(folder.unreadCount) : undefined,
      children: childrenOf(folder.id),
    }));
}

/** Custom folders nested under a role folder (e.g. Archive/Fastmail/…). */
export function buildRoleChildren(
  roleFolderId: string,
  accountFolders: MailFolder[],
): FolderTreeNode[] {
  return makeTree(accountFolders).childrenOf(roleFolderId);
}
