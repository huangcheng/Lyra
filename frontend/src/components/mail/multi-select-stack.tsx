/**
 * Multi-select reading pane: the anchor conversation on a front card with
 * stacked page edges behind it (Apple Mail style), plus the bulk action bar
 * that replaces the normal toolbar while a multi-selection is active.
 */

import {
  Archive,
  ArchiveX,
  ChevronLeft,
  ChevronRight,
  FolderInput,
  Mail,
  MailOpen,
  Star,
  StarOff,
  Trash2,
  X,
} from 'lucide-react';
import { useState, type ReactNode } from 'react';

import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { t } from '@/i18n';
import { confirmMoveToTrash } from '@/lib/confirm-trash';
import { actOnMessages, moveMessages, patchMessages } from '@/lib/conversation-actions';
import type { Conversation } from '@/lib/conversation';
import { buildAccountMoveFolderEntries, moveFolderEntryLabel } from '@/lib/folder-tree';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Front card + up to two offset pseudo-cards suggesting the stack. */
export function SelectionStackFrame({ count, children }: { count: number; children: ReactNode }) {
  return (
    <div className="relative flex min-h-0 flex-1 flex-col pt-3">
      {count > 2 ? (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-x-4 top-0 h-3 rounded-t-xl border border-b-0 border-border/50 bg-background/60"
        />
      ) : null}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-2 top-1.5 h-3 rounded-t-xl border border-b-0 border-border/60 bg-background/80"
      />
      <div className="relative flex min-h-0 flex-1 flex-col border-t border-border/60 bg-background">
        {children}
      </div>
    </div>
  );
}

/** Toolbar shown while a multi-selection is active; actions hit every selected conversation. */
export function BulkActionBar({
  convos,
  position,
  onStep,
  onError,
}: {
  /** Selected conversations in the current view order. */
  convos: Conversation[];
  /** Anchor position within the selection (for the ‹ i of N › pager). */
  position: { index: number; total: number };
  onStep: (delta: 1 | -1) => void;
  onError: (message: string | null) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const folders = useMailStore((s) => s.folders);
  const clearConversationSelection = useUIStore((s) => s.clearConversationSelection);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const applyConversationSelection = useUIStore((s) => s.applyConversationSelection);
  const selectionAnchorKey = useUIStore((s) => s.selectionAnchorKey);
  const [busy, setBusy] = useState(false);
  const [moveProgress, setMoveProgress] = useState<{ done: number; total: number } | null>(null);

  const ids = convos.flatMap((c) => c.messages.map((m) => m.id));
  const anchorConvo = convos[position.index] ?? convos[0];
  const anchorAccountId = anchorConvo?.latest.accountId;
  const anyUnread = convos.some((c) => c.unreadCount > 0);
  const anyUnstarred = convos.some((c) => !c.anyStarred);
  const inSpamFolder = anchorConvo ? folders[anchorConvo.latest.folderId]?.role === 'spam' : false;
  const moveEntries = buildAccountMoveFolderEntries(
    Object.values(folders).filter((f) => f.accountId === anchorAccountId),
    folders,
  );

  /** Removing actions with busy already held by the caller (post-confirm). */
  const runRemovingBusy = async (p: () => Promise<{ error: string | null }>) => {
    onError(null);
    const res = await p();
    setBusy(false);
    clearConversationSelection();
    setSelectedMessage(null);
    if (res.error) onError(res.error);
  };

  /** Removing actions: run, then drop the selection and the reader. */
  const runRemoving = async (p: () => Promise<{ error: string | null }>) => {
    if (busy) return;
    setBusy(true);
    await runRemovingBusy(p);
  };

  const moveTo = (folderId: string) => {
    const sameAccount = convos.filter((c) => c.latest.accountId === anchorAccountId);
    const skipped = convos.length - sameAccount.length;
    // Reported only after the batch resolves, so runRemoving's initial
    // onError(null) doesn't erase it (same pattern as the context menu).
    const skipNotice =
      skipped > 0 ? t(locale, 'mail.skippedOtherAccounts', { count: skipped }) : null;
    const moveIds = sameAccount.flatMap((c) => c.messages.map((m) => m.id));
    void runRemoving(async () => {
      setMoveProgress({ done: 0, total: moveIds.length });
      const res = await moveMessages(moveIds, folderId, (done) =>
        setMoveProgress({ done, total: moveIds.length }),
      );
      setMoveProgress(null);
      return { ...res, error: res.error ?? skipNotice };
    });
  };

  const iconClass =
    'shrink-0 rounded-[7px] text-ter-foreground hover:bg-accent hover:text-foreground';

  return (
    <div className="flex shrink-0 items-center gap-1.5 overflow-x-auto border-b border-border/60 p-2">
      <span className="px-1 text-[11px] tabular-nums text-muted-foreground">
        {t(locale, 'mail.selectionCount', { count: convos.length })}
      </span>
      <div className="flex items-center gap-0.5">
        <Button
          variant="ghost"
          size="icon"
          className={iconClass}
          disabled={busy || position.index >= position.total - 1}
          onClick={() => onStep(1)}
          aria-label={t(locale, 'mail.prevConversation')}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <span className="text-[11px] tabular-nums text-muted-foreground">
          {position.index + 1} / {position.total}
        </span>
        <Button
          variant="ghost"
          size="icon"
          className={iconClass}
          disabled={busy || position.index <= 0}
          onClick={() => onStep(-1)}
          aria-label={t(locale, 'mail.nextConversation')}
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
      <div className="mx-1 h-4 w-px bg-border/60" />
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, 'mail.archive')}
        aria-label={t(locale, 'mail.archive')}
        onClick={() => void runRemoving(() => actOnMessages(ids, 'archive'))}
      >
        <Archive className="h-4 w-4" />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, inSpamFolder ? 'mail.notSpam' : 'mail.moveToJunk')}
        aria-label={t(locale, inSpamFolder ? 'mail.notSpam' : 'mail.moveToJunk')}
        onClick={() =>
          void runRemoving(() => actOnMessages(ids, inSpamFolder ? 'notSpam' : 'spam'))
        }
      >
        <ArchiveX className="h-4 w-4" />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, 'mail.moveToTrash')}
        aria-label={t(locale, 'mail.moveToTrash')}
        onClick={() => {
          if (busy) return;
          setBusy(true);
          void (async () => {
            if (!(await confirmMoveToTrash(locale, ids.length))) {
              setBusy(false);
              return;
            }
            // runRemovingBusy manages busy from here on
            await runRemovingBusy(() => actOnMessages(ids, 'trash'));
          })();
        }}
      >
        <Trash2 className="h-4 w-4" />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, anyUnread ? 'mail.markRead' : 'mail.markUnread')}
        aria-label={t(locale, anyUnread ? 'mail.markRead' : 'mail.markUnread')}
        onClick={() => {
          if (busy) return;
          setBusy(true);
          void patchMessages(ids, { isRead: anyUnread }).then((res) => {
            setBusy(false);
            if (res.error) onError(res.error);
          });
        }}
      >
        {anyUnread ? <MailOpen className="h-4 w-4" /> : <Mail className="h-4 w-4" />}
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')}
        aria-label={t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')}
        onClick={() => {
          if (busy) return;
          setBusy(true);
          void patchMessages(ids, { isStarred: anyUnstarred }).then((res) => {
            setBusy(false);
            if (res.error) onError(res.error);
          });
        }}
      >
        {anyUnstarred ? <Star className="h-4 w-4" /> : <StarOff className="h-4 w-4" />}
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className={iconClass}
            disabled={busy}
            title={t(locale, 'mail.moveToFolder')}
            aria-label={t(locale, 'mail.moveToFolder')}
          >
            <FolderInput className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="max-h-64 w-56 overflow-y-auto">
          {moveEntries.length === 0 ? (
            <DropdownMenuLabel>{t(locale, 'mail.noFolders')}</DropdownMenuLabel>
          ) : (
            moveEntries.map((f) => (
              <DropdownMenuItem
                key={f.id}
                onSelect={() => moveTo(f.id)}
                style={{ paddingLeft: `${0.5 + f.depth * 0.75}rem` }}
              >
                <span className="truncate">{moveFolderEntryLabel(f, locale)}</span>
              </DropdownMenuItem>
            ))
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      <div className="mx-1 h-4 w-px bg-border/60" />
      <Button
        variant="ghost"
        size="icon"
        className={iconClass}
        disabled={busy}
        title={t(locale, 'mail.clearSelection')}
        aria-label={t(locale, 'mail.clearSelection')}
        onClick={() => {
          const anchor = selectionAnchorKey;
          if (anchor) {
            const keep = convos.find((c) => c.key === anchor);
            if (keep) {
              const target = keep.messages.find((m) => !m.isRead) ?? keep.latest;
              applyConversationSelection({ keys: [anchor], anchor, focus: anchor }, target.id);
              return;
            }
          }
          clearConversationSelection();
        }}
      >
        <X className="h-4 w-4" />
      </Button>
      {moveProgress ? (
        <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
          {t(locale, 'mail.movingMessages', { done: moveProgress.done, total: moveProgress.total })}
        </span>
      ) : null}
    </div>
  );
}
