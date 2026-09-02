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
import { useMemo, type CSSProperties, type HTMLAttributes, type Ref } from 'react';
import { useDndContext, useDroppable } from '@dnd-kit/core';
import { SortableContext, useSortable, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';

import type { FolderDropData, UnifiedRoleDropData } from '@/components/mail/mail-dnd';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { orderAccounts } from '@/lib/account-order';
import {
  canDropConversation,
  resolveRoleFolder,
  type ConversationDragData,
} from '@/lib/conversation-actions';
import { buildCustomFolderTree, buildRoleChildren, type FolderTreeNode } from '@/lib/folder-tree';
import { ALL_ACCOUNTS, type StandardFolderRole } from '@/lib/mail-api';
import { avatarTone, cn } from '@/lib/utils';
import { useMailStore, type UnifiedFolder } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailAccount, MailFolder } from '@/types';

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
    <div className="px-2.5 pb-1 pt-3.5 text-[10.5px] font-semibold uppercase tracking-[0.8px] text-muted-foreground">
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

function selectAccountFolder(_accountId: string, folderId: string) {
  // Folder ids are globally unique: show the folder without switching the
  // account selector (macOS Mail behavior in the unified sidebar).
  useUIStore.getState().setSelectedFolder(folderId);
}

/** Drop-target state for a folder row during a conversation drag. */
function useFolderDropTarget(drop: FolderDropData | UnifiedRoleDropData, dropId: string) {
  const { active } = useDndContext();
  const drag = active?.data.current as ConversationDragData | undefined;
  const isConvoDrag = drag?.type === 'conversation';

  let enabled = false;
  if (isConvoDrag && drag) {
    if ('unified' in drop && drop.unified) {
      const target = resolveRoleFolder(useMailStore.getState().folders, drag.accountId, drop.role);
      enabled = target !== null && !drag.folderIds.includes(target.id);
    } else {
      enabled = canDropConversation(drag, drop as FolderDropData);
    }
  }

  const { isOver, setNodeRef } = useDroppable({ id: dropId, data: drop, disabled: !enabled });
  const rowClass = isConvoDrag
    ? enabled
      ? isOver
        ? 'bg-accent ring-1 ring-ring/40'
        : undefined
      : 'opacity-40'
    : undefined;
  return { setNodeRef, rowClass };
}

function UnifiedRow({ folder, active }: { folder: UnifiedFolder; active: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const Icon = ROLE_ICONS[folder.role];
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', unified: true, role: folder.role },
    `drop:unified:${folder.role}`,
  );
  return (
    <div ref={setNodeRef} className={cn('rounded-[7px]', rowClass)}>
      <button
        type="button"
        onClick={() => selectUnifiedRole(folder.role)}
        className={cn(
          'flex h-8 w-full items-center gap-2 rounded-[7px] border px-2.5 text-left text-[13px]',
          active ? 'border-input bg-card shadow-whisper' : 'border-transparent hover:bg-accent/60',
        )}
      >
        <Icon
          className={cn('size-4 shrink-0', active ? 'text-foreground' : 'text-ter-foreground')}
        />
        <span className="truncate">{t(locale, `mail.folder.${folder.role}`)}</span>
        <UnreadCount count={folder.unreadCount} />
      </button>
    </div>
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
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', accountId, folderId: node.id },
    `drop:folder:${node.id}`,
  );

  return (
    <div>
      <div
        ref={setNodeRef}
        className={cn(
          'flex items-center rounded-[7px] border',
          active ? 'border-input bg-card shadow-whisper' : 'border-transparent hover:bg-accent/60',
          rowClass,
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
          <span
            aria-hidden
            className={cn('size-2 shrink-0 rounded-[3px]', avatarTone(node.title))}
          />
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

/** One role-folder row (plus its child folders) — a component so the drop hook is legal. */
function RoleFolderRow({
  account,
  folder,
  accountFolders,
  selectedFolderId,
  expandedIds,
  onToggleExpanded,
}: {
  account: MailAccount;
  folder: MailFolder;
  accountFolders: MailFolder[];
  selectedFolderId: string | null;
  expandedIds: Set<string>;
  onToggleExpanded: (id: string) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const role = folder.role as StandardFolderRole;
  const Icon = ROLE_ICONS[role] ?? Folder;
  const children = buildRoleChildren(folder.id, accountFolders);
  const hasChildren = children.length > 0;
  const childrenExpanded = expandedIds.has(folder.id);
  const active = selectedFolderId === folder.id;
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', accountId: account.id, folderId: folder.id },
    `drop:folder:${folder.id}`,
  );

  return (
    <div>
      <div
        ref={setNodeRef}
        className={cn(
          'flex items-center rounded-[7px] border',
          active ? 'border-input bg-card shadow-whisper' : 'border-transparent hover:bg-accent/60',
          rowClass,
        )}
      >
        {hasChildren ? (
          <button
            type="button"
            className="flex size-5 shrink-0 items-center justify-center text-ter-foreground"
            onClick={() => onToggleExpanded(folder.id)}
            aria-expanded={childrenExpanded}
            aria-label={
              childrenExpanded ? t(locale, 'mail.collapseFolder') : t(locale, 'mail.expandFolder')
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
          <Icon
            className={cn('size-4 shrink-0', active ? 'text-foreground' : 'text-ter-foreground')}
          />
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
              selectedFolderId={selectedFolderId}
              expandedIds={expandedIds}
              toggleExpanded={onToggleExpanded}
            />
          ))
        : null}
    </div>
  );
}

function AccountSection({
  account,
  selectedFolderId,
  bare = false,
  dragHandleProps,
  headerRef,
  headerStyle,
  headerClassName,
  activatorRef,
}: {
  account: MailAccount;
  selectedFolderId: string | null;
  /** Single-account view: header omitted (the switcher already names the account). */
  bare?: boolean;
  /** dnd-kit listeners/attributes for the account header (unified view only). */
  dragHandleProps?: HTMLAttributes<HTMLButtonElement>;
  /** Sortable node lands on a wrapper of the header row only — an expanded
   *  section's folder droppables must never eclipse sibling headers in
   *  collision detection, or expanded accounts cannot be reordered. */
  headerRef?: Ref<HTMLDivElement>;
  headerStyle?: CSSProperties;
  headerClassName?: string;
  /** dnd-kit activator (the button itself) for correct drag measurement. */
  activatorRef?: Ref<HTMLButtonElement>;
}) {
  // Subscribe to `folders` so this section re-renders on folder updates.
  const folders = useMailStore((s) => s.folders);
  const getFoldersForAccount = useMailStore((s) => s.getFoldersForAccount);
  // Expansion state lives in the UI store so it persists server-side.
  const expansion = useUIStore((s) => s.folderExpansion[account.id]);
  const setAccountExpanded = useUIStore((s) => s.setAccountExpanded);
  const toggleExpanded = useUIStore((s) => s.toggleFolderExpanded);
  const expanded = expansion?.expanded ?? true;
  const expandedIds = useMemo(() => new Set(expansion?.folderIds ?? []), [expansion]);

  const accountFolders = getFoldersForAccount(account.id);
  const roleFolders = accountFolders.filter((folder) => folder.role);
  const customTree = buildCustomFolderTree(accountFolders, folders);
  const totalUnread = accountFolders.reduce((sum, folder) => sum + folder.unreadCount, 0);

  const onToggleExpanded = (id: string) => toggleExpanded(account.id, id);

  return (
    <div>
      {bare ? null : (
        <div ref={headerRef} style={headerStyle} className={headerClassName}>
          <button
            ref={activatorRef}
            type="button"
            onClick={() => setAccountExpanded(account.id, !expanded)}
            aria-expanded={expanded}
            {...dragHandleProps}
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
        </div>
      )}
      {bare || expanded ? (
        <div className={bare ? undefined : 'pl-4'}>
          {roleFolders.map((folder) => (
            <RoleFolderRow
              key={folder.id}
              account={account}
              folder={folder}
              accountFolders={accountFolders}
              selectedFolderId={selectedFolderId}
              expandedIds={expandedIds}
              onToggleExpanded={onToggleExpanded}
            />
          ))}
          {customTree.map((node) => (
            <CustomFolderBranch
              key={node.id}
              node={node}
              depth={0}
              accountId={account.id}
              selectedFolderId={selectedFolderId}
              expandedIds={expandedIds}
              toggleExpanded={onToggleExpanded}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function SortableAccountSection({
  account,
  selectedFolderId,
}: {
  account: MailAccount;
  selectedFolderId: string | null;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    setActivatorNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: account.id });
  return (
    <AccountSection
      account={account}
      selectedFolderId={selectedFolderId}
      // touch-action: none keeps the browser from hijacking vertical scroll,
      // which would pointercancel and abort the drag (dnd-kit guidance).
      dragHandleProps={{ ...attributes, ...listeners, style: { touchAction: 'none' } }}
      headerRef={setNodeRef}
      headerStyle={{ transform: CSS.Transform.toString(transform), transition }}
      // The overlay chip carries the affordance; the ghost stays faint.
      headerClassName={isDragging ? 'opacity-40' : undefined}
      activatorRef={setActivatorNodeRef}
    />
  );
}

/** ACCOUNTS section with drag-to-reorder; the DndContext lives in mail.tsx. */
function SortableAccountSections({
  accounts,
  selectedFolderId,
}: {
  accounts: MailAccount[];
  selectedFolderId: string | null;
}) {
  const accountOrder = useUIStore((s) => s.accountOrder);
  const ordered = orderAccounts(accounts, accountOrder);
  const ids = ordered.map((a) => a.id);

  return (
    <SortableContext items={ids} strategy={verticalListSortingStrategy}>
      <div className="grid gap-0.5">
        {ordered.map((account) => (
          <SortableAccountSection
            key={account.id}
            account={account}
            selectedFolderId={selectedFolderId}
          />
        ))}
      </div>
    </SortableContext>
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
                  'flex h-9 w-9 items-center justify-center rounded-[7px] border',
                  active
                    ? 'border-input bg-card text-foreground shadow-whisper'
                    : 'border-transparent text-ter-foreground hover:bg-accent/60 hover:text-foreground',
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
          <AccountSection account={selectedAccount} selectedFolderId={selectedFolderId} bare />
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
      <SortableAccountSections accounts={accounts} selectedFolderId={selectedFolderId} />
    </div>
  );
}
