/**
 * Build a nested folder tree for the sidebar from flat folder rows.
 */

import type { StandardFolderRole } from '@/lib/mail-api';
import type { MailFolder } from '@/types';

export interface FolderTreeNode {
  id: string;
  title: string;
  label?: string;
  children: FolderTreeNode[];
}

/** Apple Mail per-account role folder order. */
export const CANONICAL_ROLE_ORDER: StandardFolderRole[] = [
  'inbox',
  'drafts',
  'sent',
  'spam',
  'trash',
  'archive',
];

function sortFolders(a: MailFolder, b: MailFolder): number {
  return a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
}

export function effectiveFolderRole(folder: MailFolder): StandardFolderRole | undefined {
  return (folder.roleOverride ?? folder.role) as StandardFolderRole | undefined;
}

/** Sort role folders Inbox → Drafts → Sent → Junk → Trash → Archive. */
export function sortRoleFolders(folders: MailFolder[]): MailFolder[] {
  return [...folders].sort((a, b) => {
    const ar = effectiveFolderRole(a);
    const br = effectiveFolderRole(b);
    const ai = ar ? CANONICAL_ROLE_ORDER.indexOf(ar) : CANONICAL_ROLE_ORDER.length;
    const bi = br ? CANONICAL_ROLE_ORDER.indexOf(br) : CANONICAL_ROLE_ORDER.length;
    if (ai !== bi) return ai - bi;
    return sortFolders(a, b);
  });
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

/** One row in a move-to picker: same nesting as the sidebar, flattened for menus. */
export interface MoveFolderEntry {
  id: string;
  name: string;
  role?: string | null;
  depth: number;
}

function flattenTree(nodes: FolderTreeNode[], depth: number): MoveFolderEntry[] {
  const rows: MoveFolderEntry[] = [];
  for (const node of nodes) {
    rows.push({ id: node.id, name: node.title, depth });
    rows.push(...flattenTree(node.children, depth + 1));
  }
  return rows;
}

/**
 * Move-to destination list for one account: role folders (with nested
 * children), then the custom folder tree — never a cross-account or
 * alphabetically flattened dump.
 */
export function buildAccountMoveFolderEntries(
  accountFolders: MailFolder[],
  allFolders: Record<string, MailFolder>,
): MoveFolderEntry[] {
  const rows: MoveFolderEntry[] = [];
  const roleFolders = sortRoleFolders(accountFolders.filter((folder) => folder.role));

  for (const role of roleFolders) {
    rows.push({
      id: role.id,
      name: role.name,
      role: role.role,
      depth: 0,
    });
    rows.push(...flattenTree(buildRoleChildren(role.id, accountFolders), 1));
  }

  rows.push(...flattenTree(buildCustomFolderTree(accountFolders, allFolders), 0));
  return rows;
}
