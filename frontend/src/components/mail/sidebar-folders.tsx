/**
 * Mail sidebar folder sections (redesign v2).
 *
 * UNIFIED smart mailboxes on top, then collapsible per-account folder
 * trees (macOS-Mail style): role folders first, custom folders nested by
 * parentId with 16px indent per level.
 */

import {
  Archive,
  ChevronDown,
  ChevronRight,
  File,
  Flag,
  Folder,
  Inbox,
  Send,
  Trash2,
  type LucideIcon,
} from 'lucide-react';
import { useState } from 'react';

import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { buildCustomFolderTree, buildRoleChildren, type FolderTreeNode } from '@/lib/folder-tree';
import { ALL_ACCOUNTS, type StandardFolderRole } from '@/lib/mail-api';
import { cn } from '@/lib/utils';
import { useMailStore, type UnifiedFolder } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailAccount } from '@/types';

const ROLE_ICONS: Record<StandardFolderRole, LucideIcon> = {
  inbox: Inbox,
  drafts: File,
  sent: Send,
  trash: Trash2,
  spam: Flag,
  archive: Archive,
};

/** UNIFIED display order: core roles only; spam/archive live under each account. */
const UNIFIED_ROLE_ORDER: StandardFolderRole[] = ['inbox', 'drafts', 'sent', 'trash'];

function SectionLabel({ children }: { children: string }) {
  return (
    <div className="px-2.5 pb-1 pt-3.5 text-[10.5px] font-semibold uppercase tracking-[0.8px] text-ter-foreground">
      {children}
    </div>
  );
}

function UnreadCount({ count }: { count: number }) {
  if (count <= 0) return null;
  return <span className="ml-auto text-[11.5px] tabular-nums text-muted-foreground">{count}</span>;
}

function selectUnifiedRole(role: StandardFolderRole) {
  const { setSelectedAccount, setSelectedFolderRole } = useUIStore.getState();
  setSelectedAccount(ALL_ACCOUNTS);
  setSelectedFolderRole(role);
}

function selectAccountFolder(accountId: string, folderId: string) {
  const { setSelectedAccount, setSelectedFolder } = useUIStore.getState();
  setSelectedAccount(accountId);
  setSelectedFolder(folderId);
}

function UnifiedRow({ folder, active }: { folder: UnifiedFolder; active: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const Icon = ROLE_ICONS[folder.role];
  return (
    <button
      type="button"
      onClick={() => selectUnifiedRole(folder.role)}
      className={cn(
        'flex h-8 w-full items-center gap-2 rounded-[7px] px-2.5 text-left text-[13px]',
        active ? 'bg-accent' : 'hover:bg-accent/60',
      )}
    >
      <Icon className="size-4 shrink-0" />
      <span className="truncate">{t(locale, `mail.folder.${folder.role}`)}</span>
      <UnreadCount count={folder.unreadCount} />
    </button>
  );
}

function CustomFolderBranch({
  node,
  depth,
  accountId,
  selectedFolderId,
  expandedIds,
  toggleExpanded,
}: {
  node: FolderTreeNode;
  depth: number;
  accountId: string;
  selectedFolderId: string | null;
  expandedIds: Set<string>;
  toggleExpanded: (id: string) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const hasChildren = node.children.length > 0;
  const expanded = expandedIds.has(node.id);
  const active = selectedFolderId === node.id;

  return (
    <div>
      <div
        className={cn(
          'flex items-center rounded-[7px]',
          active ? 'bg-accent' : 'hover:bg-accent/60',
        )}
        style={{ marginLeft: depth * 16 }}
      >
        {hasChildren ? (
          <button
            type="button"
            className="flex size-5 shrink-0 items-center justify-center text-ter-foreground"
            onClick={() => toggleExpanded(node.id)}
            aria-expanded={expanded}
            aria-label={
              expanded ? t(locale, 'mail.collapseFolder') : t(locale, 'mail.expandFolder')
            }
          >
            {expanded ? (
              <ChevronDown className="size-3.5" />
            ) : (
              <ChevronRight className="size-3.5" />
            )}
          </button>
        ) : (
          <span className="inline-block size-5 shrink-0" />
        )}
        <button
          type="button"
          className="flex h-8 min-w-0 flex-1 items-center gap-2 pr-2.5 text-left text-[13px]"
          onClick={() => selectAccountFolder(accountId, node.id)}
        >
          <Folder className="size-4 shrink-0" />
          <span className="truncate">{node.title}</span>
          {node.label ? (
            <span className="ml-auto text-[11.5px] tabular-nums text-muted-foreground">
              {node.label}
            </span>
          ) : null}
        </button>
      </div>
      {hasChildren && expanded
        ? node.children.map((child) => (
            <CustomFolderBranch
              key={child.id}
              node={child}
              depth={depth + 1}
              accountId={accountId}
              selectedFolderId={selectedFolderId}
              expandedIds={expandedIds}
              toggleExpanded={toggleExpanded}
            />
          ))
        : null}
    </div>
  );
}

function AccountSection({
  account,
  selectedAccountId,
  selectedFolderId,
  bare = false,
}: {
  account: MailAccount;
  selectedAccountId: string;
  selectedFolderId: string | null;
  /** Single-account view: header omitted (the switcher already names the account). */
  bare?: boolean;
}) {
  const locale = useUIStore((s) => s.locale);
  // Subscribe to `folders` so this section re-renders on folder updates.
  const folders = useMailStore((s) => s.folders);
  const getFoldersForAccount = useMailStore((s) => s.getFoldersForAccount);
  const [expanded, setExpanded] = useState(true);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(() => new Set());

  const accountFolders = getFoldersForAccount(account.id);
  const roleFolders = accountFolders.filter((folder) => folder.role);
  const customTree = buildCustomFolderTree(accountFolders, folders);
  const totalUnread = accountFolders.reduce((sum, folder) => sum + folder.unreadCount, 0);

  const toggleExpanded = (id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const isSelectedAccount = selectedAccountId === account.id;

  return (
    <div>
      {bare ? null : (
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          aria-expanded={expanded}
          className="flex h-8 w-full items-center gap-1.5 rounded-[7px] px-2.5 hover:bg-accent/60"
        >
          {expanded ? (
            <ChevronDown className="size-3.5 shrink-0 text-ter-foreground" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0 text-ter-foreground" />
          )}
          <span
            className="truncate text-[12.5px] font-semibold"
            title={account.displayName || account.emailAddress}
          >
            {account.displayName || account.emailAddress}
          </span>
          <UnreadCount count={totalUnread} />
        </button>
      )}
      {bare || expanded ? (
        <div className={bare ? undefined : 'pl-4'}>
          {roleFolders.map((folder) => {
            const role = folder.role as StandardFolderRole;
            const Icon = ROLE_ICONS[role] ?? Folder;
            const children = buildRoleChildren(folder.id, accountFolders);
            const hasChildren = children.length > 0;
            const childrenExpanded = expandedIds.has(folder.id);
            const active = isSelectedAccount && selectedFolderId === folder.id;
            return (
              <div key={folder.id}>
                <div
                  className={cn(
                    'flex items-center rounded-[7px]',
                    active ? 'bg-accent' : 'hover:bg-accent/60',
                  )}
                >
                  {hasChildren ? (
                    <button
                      type="button"
                      className="flex size-5 shrink-0 items-center justify-center text-ter-foreground"
                      onClick={() => toggleExpanded(folder.id)}
                      aria-expanded={childrenExpanded}
                      aria-label={
                        childrenExpanded
                          ? t(locale, 'mail.collapseFolder')
                          : t(locale, 'mail.expandFolder')
                      }
                    >
                      {childrenExpanded ? (
                        <ChevronDown className="size-3.5" />
                      ) : (
                        <ChevronRight className="size-3.5" />
                      )}
                    </button>
                  ) : (
                    <span className="inline-block size-5 shrink-0" />
                  )}
                  <button
                    type="button"
                    onClick={() => selectAccountFolder(account.id, folder.id)}
                    className="flex h-8 min-w-0 flex-1 items-center gap-2 pr-2.5 text-left text-[13px]"
                  >
                    <Icon className="size-4 shrink-0" />
                    <span className="truncate">{t(locale, `mail.folder.${role}`)}</span>
                    <UnreadCount count={folder.unreadCount} />
                  </button>
                </div>
                {hasChildren && childrenExpanded
                  ? children.map((child) => (
                      <CustomFolderBranch
                        key={child.id}
                        node={child}
                        depth={1}
                        accountId={account.id}
                        selectedFolderId={isSelectedAccount ? selectedFolderId : null}
                        expandedIds={expandedIds}
                        toggleExpanded={toggleExpanded}
                      />
                    ))
                  : null}
              </div>
            );
          })}
          {customTree.map((node) => (
            <CustomFolderBranch
              key={node.id}
              node={node}
              depth={0}
              accountId={account.id}
              selectedFolderId={isSelectedAccount ? selectedFolderId : null}
              expandedIds={expandedIds}
              toggleExpanded={toggleExpanded}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** Collapsed-pane fallback: icon-only rows with tooltips (Nav parity). */
function CollapsedFolders({ unifiedFolders }: { unifiedFolders: UnifiedFolder[] }) {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);

  return (
    <nav className="grid justify-center gap-1 px-2 py-2">
      {unifiedFolders.map((folder) => {
        const Icon = ROLE_ICONS[folder.role];
        const title = t(locale, `mail.folder.${folder.role}`);
        const active = selectedAccountId === ALL_ACCOUNTS && selectedFolderRole === folder.role;
        return (
          <Tooltip key={folder.role} delayDuration={0}>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => selectUnifiedRole(folder.role)}
                className={cn(
                  'flex h-9 w-9 items-center justify-center rounded-[7px]',
                  active ? 'bg-accent' : 'hover:bg-accent/60',
                )}
              >
                <Icon className="h-4 w-4" />
                <span className="sr-only">{title}</span>
              </button>
            </TooltipTrigger>
            <TooltipContent side="right" className="flex items-center gap-4">
              {title}
              {folder.unreadCount > 0 ? (
                <span className="ml-auto text-muted-foreground">{folder.unreadCount}</span>
              ) : null}
            </TooltipContent>
          </Tooltip>
        );
      })}
    </nav>
  );
}

export function SidebarFolders({ isCollapsed }: { isCollapsed: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const accounts = useMailStore((s) => s.accounts);
  // Subscribe to `folders` so the unified counts re-render on folder updates.
  useMailStore((s) => s.folders);
  const getUnifiedFolders = useMailStore((s) => s.getUnifiedFolders);

  const returned = getUnifiedFolders();
  const unifiedFolders = UNIFIED_ROLE_ORDER.map((role) =>
    returned.find((f) => f.role === role),
  ).filter((folder): folder is UnifiedFolder => Boolean(folder));

  if (isCollapsed) {
    return <CollapsedFolders unifiedFolders={unifiedFolders} />;
  }

  // A specific account is selected in the switcher: show only its tree.
  const selectedAccount = accounts.find((a) => a.id === selectedAccountId);
  if (selectedAccount) {
    return (
      <div className="flex flex-col px-2 pb-2">
        <div className="grid gap-0.5 pt-2">
          <AccountSection
            account={selectedAccount}
            selectedAccountId={selectedAccountId}
            selectedFolderId={selectedFolderId}
            bare
          />
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col px-2 pb-2">
      <SectionLabel>{t(locale, 'mail.section.unified')}</SectionLabel>
      <div className="grid gap-0.5">
        {unifiedFolders.map((folder) => (
          <UnifiedRow
            key={folder.role}
            folder={folder}
            active={selectedAccountId === ALL_ACCOUNTS && selectedFolderRole === folder.role}
          />
        ))}
      </div>
      <SectionLabel>{t(locale, 'mail.section.accounts')}</SectionLabel>
      <div className="grid gap-0.5">
        {accounts.map((account) => (
          <AccountSection
            key={account.id}
            account={account}
            selectedAccountId={selectedAccountId}
            selectedFolderId={selectedFolderId}
          />
        ))}
      </div>
    </div>
  );
}
