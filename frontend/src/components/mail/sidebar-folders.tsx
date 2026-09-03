/**
 * Mail sidebar folder sections (redesign v2 + Apple Mail Favorites).
 *
 * Favorites on top (All Inboxes expandable, Starred, unified Drafts/Sent/Trash),
 * then collapsible per-account folder trees. Role folders under accounts remain
 * even when they also appear in Favorites.
 */

import {
  Archive,
  ChevronDown,
  ChevronRight,
  File,
  Flag,
  Folder,
  Inbox,
  Loader2,
  Send,
  Star,
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
import { accountInboxChildren, starredCount } from '@/lib/favorites-sidebar';
import { buildCustomFolderTree, buildRoleChildren, type FolderTreeNode } from '@/lib/folder-tree';
import { ALL_ACCOUNTS, type StandardFolderRole } from '@/lib/mail-api';
import { useSyncProgress } from '@/lib/sync-progress';
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

/** Favorites role rows after All Inboxes + Starred. */
const FAVORITES_ROLE_TAIL: StandardFolderRole[] = ['drafts', 'sent', 'trash'];

function SectionLabel({ children }: { children: string }) {
  return (
    <div className="px-2.5 pb-1 pt-3.5 text-[10.5px] font-semibold uppercase tracking-[0.8px] text-muted-foreground">
      {children}
    </div>
  );
}

function UnreadCount({ count }: { count: number }) {
  // Always reserve the badge slot so 0↔N count flips don't shift the row.
  return (
    <span className="ml-auto min-w-[1.25rem] shrink-0 text-right text-[11.5px] tabular-nums text-muted-foreground">
      {count > 0 ? count : '\u00a0'}
    </span>
  );
}

function selectUnifiedRole(role: StandardFolderRole) {
  const { setSelectedAccount, setSelectedFolderRole } = useUIStore.getState();
  setSelectedAccount(ALL_ACCOUNTS);
  setSelectedFolderRole(role);
}

function selectStarred() {
  // Keep the account switcher scope (single-account → that account's starred).
  useUIStore.getState().setSelectedFolderRole('starred');
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
        ? 'bg-accent/80'
        : undefined
      : 'opacity-40'
    : undefined;
  return { setNodeRef, rowClass };
}

/** Selected folder: cool wash only — no card border / whisper ring (those read as mud + hard edges). */
function navRowClass(active: boolean): string {
  return cn(
    'flex w-full min-w-0 max-w-full items-center rounded-[7px] text-left text-[13px] transition-colors',
    active ? 'bg-accent font-medium text-foreground' : 'text-foreground hover:bg-accent/50',
  );
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
        className={cn(navRowClass(active), 'h-8 gap-2 px-2.5')}
      >
        <Icon
          className={cn('size-4 shrink-0', active ? 'text-foreground' : 'text-ter-foreground')}
        />
        <span className="truncate">
          {folder.role === 'inbox'
            ? t(locale, 'nav.allInboxes')
            : t(locale, `mail.folder.${folder.role}`)}
        </span>
        <UnreadCount count={folder.unreadCount} />
      </button>
    </div>
  );
}

function AllInboxesFavorite({
  folder,
  accounts,
  folders,
}: {
  folder: UnifiedFolder;
  accounts: MailAccount[];
  folders: Record<string, MailFolder>;
}) {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const accountOrder = useUIStore((s) => s.accountOrder);
  const expandedPref = useUIStore((s) => s.favoritesAllInboxesExpanded);
  const setExpanded = useUIStore((s) => s.setFavoritesAllInboxesExpanded);

  const ordered = orderAccounts(accounts, accountOrder);
  const children = accountInboxChildren(ordered, folders);
  const expanded = expandedPref ?? ordered.length >= 2;
  const parentActive =
    selectedAccountId === ALL_ACCOUNTS &&
    selectedFolderRole === 'inbox' &&
    !selectedFolderId;

  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', unified: true, role: 'inbox' },
    'drop:unified:inbox',
  );

  return (
    <div className="min-w-0">
      <div ref={setNodeRef} className={cn('rounded-[7px]', rowClass)}>
        <div className={cn(navRowClass(parentActive), 'h-8')}>
          {children.length > 0 ? (
            <button
              type="button"
              className="flex size-5 shrink-0 items-center justify-center text-ter-foreground"
              onClick={() => setExpanded(!expanded)}
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
            onClick={() => selectUnifiedRole('inbox')}
            className="flex h-8 min-w-0 flex-1 items-center gap-2 pr-2.5 text-left text-[13px]"
          >
            <Inbox
              className={cn(
                'size-4 shrink-0',
                parentActive ? 'text-foreground' : 'text-ter-foreground',
              )}
            />
            <span className="truncate">{t(locale, 'nav.allInboxes')}</span>
            <UnreadCount count={folder.unreadCount} />
          </button>
        </div>
      </div>
      {expanded
        ? children.map((child) => {
            const active = selectedFolderId === child.folderId;
            return (
              <div key={child.folderId} className="min-w-0" style={{ paddingLeft: 16 }}>
                <button
                  type="button"
                  onClick={() => selectAccountFolder(child.accountId, child.folderId)}
                  className={cn(navRowClass(active), 'h-8 gap-2 px-2.5')}
                >
                  <Inbox
                    className={cn(
                      'size-4 shrink-0',
                      active ? 'text-foreground' : 'text-ter-foreground',
                    )}
                  />
                  <span className="truncate">{child.accountLabel}</span>
                  <UnreadCount count={child.unreadCount} />
                </button>
              </div>
            );
          })
        : null}
    </div>
  );
}

function StarredFavoriteRow({ count }: { count: number }) {
  const locale = useUIStore((s) => s.locale);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const active = selectedFolderRole === 'starred' && !selectedFolderId;
  return (
    <button
      type="button"
      onClick={() => selectStarred()}
      className={cn(navRowClass(active), 'h-8 gap-2 px-2.5')}
    >
      <Star
        className={cn('size-4 shrink-0', active ? 'text-foreground' : 'text-ter-foreground')}
      />
      <span className="truncate">{t(locale, 'nav.starred')}</span>
      <UnreadCount count={count} />
    </button>
  );
}

function FavoritesBlock({
  unifiedFolders,
  accounts,
}: {
  unifiedFolders: UnifiedFolder[];
  accounts: MailAccount[];
}) {
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const folders = useMailStore((s) => s.folders);
  const messages = useMailStore((s) => s.messages);
  const inbox = unifiedFolders.find((f) => f.role === 'inbox');
  const tail = FAVORITES_ROLE_TAIL.map((role) => unifiedFolders.find((f) => f.role === role)).filter(
    (folder): folder is UnifiedFolder => Boolean(folder),
  );
  const stars = starredCount(messages, selectedAccountId, ALL_ACCOUNTS);

  return (
    <>
      {inbox ? (
        <AllInboxesFavorite folder={inbox} accounts={accounts} folders={folders} />
      ) : null}
      <StarredFavoriteRow count={stars} />
      {tail.map((folder) => (
        <UnifiedRow
          key={folder.role}
          folder={folder}
          active={selectedAccountId === ALL_ACCOUNTS && selectedFolderRole === folder.role}
        />
      ))}
    </>
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
    <div className="min-w-0">
      <div
        ref={setNodeRef}
        className={cn(navRowClass(active), rowClass)}
        style={{ paddingLeft: depth * 16 }}
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
    <div className="min-w-0">
      <div ref={setNodeRef} className={cn(navRowClass(active), rowClass)}>
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
  // Default collapsed: unified folders first; accounts open on demand (or from persisted ui_state).
  const expanded = expansion?.expanded ?? false;
  const expandedIds = useMemo(() => new Set(expansion?.folderIds ?? []), [expansion]);

  const accountFolders = getFoldersForAccount(account.id);
  const roleFolders = accountFolders.filter((folder) => folder.role);
  const customTree = buildCustomFolderTree(accountFolders, folders);
  const totalUnread = accountFolders.reduce((sum, folder) => sum + folder.unreadCount, 0);

  const onToggleExpanded = (id: string) => toggleExpanded(account.id, id);
  const isSyncing = useSyncProgress().get(account.id)?.state === 'syncing';

  return (
    <div className="min-w-0">
      {bare ? null : (
        <div ref={headerRef} style={headerStyle} className={cn('min-w-0', headerClassName)}>
          <button
            ref={activatorRef}
            type="button"
            onClick={() => setAccountExpanded(account.id, !expanded)}
            aria-expanded={expanded}
            {...dragHandleProps}
            className="flex h-8 w-full min-w-0 items-center gap-1.5 rounded-[7px] px-2.5 hover:bg-accent/60"
          >
            {expanded ? (
              <ChevronDown className="size-3.5 shrink-0 text-ter-foreground" />
            ) : (
              <ChevronRight className="size-3.5 shrink-0 text-ter-foreground" />
            )}
            {isSyncing ? (
              <Loader2 className="size-3 shrink-0 animate-spin text-ter-foreground" />
            ) : null}
            <span
              className="min-w-0 truncate text-[12.5px] font-semibold"
              title={account.displayName || account.emailAddress}
            >
              {account.displayName || account.emailAddress}
            </span>
            <UnreadCount count={totalUnread} />
          </button>
        </div>
      )}
      {bare || expanded ? (
        <div className={cn('min-w-0', bare ? undefined : 'pl-4')}>
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
  } = useSortable({
    id: account.id,
    // Folder/unread refreshes change section height; default layout animation
    // then "bounces" the whole account list. Only transform while dragging.
    animateLayoutChanges: () => false,
  });
  return (
    <AccountSection
      account={account}
      selectedFolderId={selectedFolderId}
      // touch-action: none keeps the browser from hijacking vertical scroll,
      // which would pointercancel and abort the drag (dnd-kit guidance).
      dragHandleProps={{ ...attributes, ...listeners, style: { touchAction: 'none' } }}
      headerRef={setNodeRef}
      headerStyle={{
        transform: CSS.Transform.toString(transform),
        transition: isDragging ? transition : undefined,
      }}
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
      <div className="grid min-w-0 gap-0.5">
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

/** Collapsed-pane fallback: icon-only Favorites rows with tooltips. */
function CollapsedFolders({ unifiedFolders }: { unifiedFolders: UnifiedFolder[] }) {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);

  const items: Array<{ key: string; title: string; Icon: LucideIcon; onClick: () => void; active: boolean; count: number }> =
    [];
  const inbox = unifiedFolders.find((f) => f.role === 'inbox');
  if (inbox) {
    items.push({
      key: 'inbox',
      title: t(locale, 'nav.allInboxes'),
      Icon: Inbox,
      onClick: () => selectUnifiedRole('inbox'),
      active: selectedAccountId === ALL_ACCOUNTS && selectedFolderRole === 'inbox',
      count: inbox.unreadCount,
    });
  }
  items.push({
    key: 'starred',
    title: t(locale, 'nav.starred'),
    Icon: Star,
    onClick: () => selectStarred(),
    active: selectedFolderRole === 'starred',
    count: 0,
  });
  for (const role of FAVORITES_ROLE_TAIL) {
    const folder = unifiedFolders.find((f) => f.role === role);
    if (!folder) continue;
    items.push({
      key: role,
      title: t(locale, `mail.folder.${role}`),
      Icon: ROLE_ICONS[role],
      onClick: () => selectUnifiedRole(role),
      active: selectedAccountId === ALL_ACCOUNTS && selectedFolderRole === role,
      count: folder.unreadCount,
    });
  }

  return (
    <nav className="grid justify-center gap-1 px-2 py-2">
      {items.map((item) => {
        const { Icon } = item;
        return (
          <Tooltip key={item.key} delayDuration={0}>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={item.onClick}
                className={cn(
                  'flex h-9 w-9 items-center justify-center rounded-[7px]',
                  item.active
                    ? 'bg-accent text-foreground'
                    : 'text-ter-foreground hover:bg-accent/50 hover:text-foreground',
                )}
              >
                <Icon className="h-4 w-4" />
                <span className="sr-only">{item.title}</span>
              </button>
            </TooltipTrigger>
            <TooltipContent side="right" className="flex items-center gap-4">
              {item.title}
              {item.count > 0 ? (
                <span className="ml-auto text-muted-foreground">{item.count}</span>
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
  const accounts = useMailStore((s) => s.accounts);
  // Subscribe to `folders` so the unified counts re-render on folder updates.
  useMailStore((s) => s.folders);
  const getUnifiedFolders = useMailStore((s) => s.getUnifiedFolders);

  const returned = getUnifiedFolders();
  const favoritesRoles: StandardFolderRole[] = ['inbox', ...FAVORITES_ROLE_TAIL];
  const unifiedFolders = favoritesRoles
    .map((role) => returned.find((f) => f.role === role))
    .filter((folder): folder is UnifiedFolder => Boolean(folder));

  if (isCollapsed) {
    return <CollapsedFolders unifiedFolders={unifiedFolders} />;
  }

  // A specific account is selected in the switcher: Favorites stay global;
  // accounts area shows only that tree.
  const selectedAccount = accounts.find((a) => a.id === selectedAccountId);

  return (
    <div className="flex min-w-0 flex-col px-2 pb-2">
      <SectionLabel>{t(locale, 'mail.section.unified')}</SectionLabel>
      <div className="grid min-w-0 gap-0.5">
        <FavoritesBlock unifiedFolders={unifiedFolders} accounts={accounts} />
      </div>
      {selectedAccount ? (
        <div className="grid min-w-0 gap-0.5 pt-2">
          <AccountSection account={selectedAccount} selectedFolderId={selectedFolderId} bare />
        </div>
      ) : (
        <>
          <SectionLabel>{t(locale, 'mail.section.accounts')}</SectionLabel>
          <SortableAccountSections accounts={accounts} selectedFolderId={selectedFolderId} />
        </>
      )}
    </div>
  );
}
