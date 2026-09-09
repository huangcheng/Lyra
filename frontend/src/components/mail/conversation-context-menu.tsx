/**
 * Right-click menu for a conversation row in the mail list.
 *
 * Every action loops the whole conversation via `lib/conversation-actions`;
 * Reply/Reply All/Forward (and Edit draft) target the latest message only.
 */

import { addDays, addHours, format, nextSaturday } from 'date-fns';
import {
  Archive,
  ArchiveX,
  Bell,
  BellOff,
  Check,
  Clock,
  Copy,
  FolderInput,
  Forward,
  Inbox,
  MailOpen,
  Mail,
  PenSquare,
  Reply,
  ReplyAll,
  Star,
  StarOff,
  Trash2,
} from 'lucide-react';
import { useEffect, useRef, useState, type ReactNode } from 'react';

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { t } from '@/i18n';
import { confirmMoveToTrash } from '@/lib/confirm-trash';
import { isThreadMuted, setThreadMuted, subscribeNotificationPrefs } from '@/lib/notifications';
import {
  actOnMessages,
  copyMessages,
  editDraftFromList,
  forwardFromList,
  moveMessages,
  patchMessages,
  replyFromList,
  snoozeMessages,
} from '@/lib/conversation-actions';
import type { Conversation } from '@/lib/conversation';
import { buildAccountMoveFolderEntries, moveFolderEntryLabel } from '@/lib/folder-tree';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Filter input that focuses itself on mount (i.e. when the submenu opens).
 *  Radix omits `onOpenAutoFocus` from SubContent props, and its own mount
 *  focus runs in a parent effect — so we focus in a rAF after it. */
function FilterInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const raf = requestAnimationFrame(() => ref.current?.focus());
    return () => cancelAnimationFrame(raf);
  }, []);
  return (
    <input
      ref={ref}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => e.stopPropagation()}
      placeholder={placeholder}
      className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-sm outline-none focus:border-ring"
    />
  );
}

/** Move/Copy submenu: same account only, nested like the sidebar tree. */
function FolderPickerSub({
  convo,
  labelKey,
  icon,
  onPick,
}: {
  convo: Conversation;
  labelKey: 'mail.moveToFolder' | 'mail.copyToFolder';
  icon: ReactNode;
  onPick: (folderId: string) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const folders = useMailStore((s) => s.folders);
  const account = useMailStore((s) => s.getAccountById(convo.latest.accountId));
  const [query, setQuery] = useState('');
  const entries = buildAccountMoveFolderEntries(
    Object.values(folders).filter((f) => f.accountId === convo.latest.accountId),
    folders,
  );
  const q = query.trim().toLowerCase();
  const shown = q
    ? entries
        .filter((e) => {
          const label = moveFolderEntryLabel(e, locale).toLowerCase();
          return label.includes(q) || e.name.toLowerCase().includes(q);
        })
        .map((e) => ({ ...e, depth: 0 }))
    : entries;
  const currentFolderIds = new Set(convo.messages.map((m) => m.folderId));

  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger>
        {icon}
        {t(locale, labelKey)}
      </ContextMenuSubTrigger>
      <ContextMenuSubContent className="w-56">
        {account ? (
          <ContextMenuLabel className="truncate">
            {account.displayName || account.emailAddress}
          </ContextMenuLabel>
        ) : null}
        <div className="px-1 pb-1">
          <FilterInput
            value={query}
            onChange={setQuery}
            placeholder={t(locale, 'mail.filterFolders')}
          />
        </div>
        <div className="max-h-64 overflow-y-auto">
          {shown.length === 0 ? (
            <ContextMenuLabel>{t(locale, 'mail.noFolders')}</ContextMenuLabel>
          ) : (
            shown.map((f) => (
              <ContextMenuItem
                key={f.id}
                disabled={currentFolderIds.has(f.id)}
                onSelect={() => onPick(f.id)}
                style={{ paddingLeft: `${0.5 + f.depth * 0.75}rem` }}
              >
                <span className="truncate">{moveFolderEntryLabel(f, locale)}</span>
                {currentFolderIds.has(f.id) ? <Check className="ml-auto" /> : null}
              </ContextMenuItem>
            ))
          )}
        </div>
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}

export function ConversationContextMenu({
  convo,
  multiConvos,
  onActionError,
  children,
}: {
  convo: Conversation;
  /** Non-null when the right-clicked row is part of a multi-selection. */
  multiConvos?: Conversation[];
  /** Surface a failure in the list's error line (null clears it). */
  onActionError: (message: string | null) => void;
  children: ReactNode;
}) {
  const locale = useUIStore((s) => s.locale);
  const folders = useMailStore((s) => s.folders);
  const latest = convo.latest;
  const targets = multiConvos && multiConvos.length > 1 ? multiConvos : [convo];
  const targetIds = targets.flatMap((c) => c.messages.map((m) => m.id));
  const anyUnread = targets.some((c) => c.unreadCount > 0);
  const anyUnstarred = targets.some((c) => !c.anyStarred);
  const today = new Date();
  // Reading a spam-folder conversation flips the junk action into its
  // opposite: "Not spam" rescues it back to the inbox and allow-learns.
  const inSpamFolder = folders[latest.folderId]?.role === 'spam';

  // Notification mute state for the conversation's thread (null when the
  // messages are not threaded yet — then mute only hides them session-locally).
  const notifyThreadId =
    latest.threadId ?? convo.messages.find((m) => m.threadId)?.threadId ?? null;
  const [notifyMuted, setNotifyMuted] = useState(() =>
    notifyThreadId ? isThreadMuted(notifyThreadId) : false,
  );
  useEffect(() => {
    if (!notifyThreadId) return;
    return subscribeNotificationPrefs(() => setNotifyMuted(isThreadMuted(notifyThreadId)));
  }, [notifyThreadId]);

  const report = (error: string | null) => onActionError(error);
  const run = (p: Promise<{ error: string | null }>) => void p.then((r) => report(r.error));
  const clearSelection = useUIStore((s) => s.clearConversationSelection);
  /** Removing actions drop the selection once the batch starts. */
  const runRemoving = (p: Promise<{ error: string | null }>) => {
    clearSelection();
    run(p);
  };
  /** Move/Copy stay account-scoped to the right-clicked convo; other-account
   *  conversations in the selection are skipped with a notice. The notice is
   *  reported only after the batch resolves, so a null (success) error from
   *  the batch doesn't erase it. */
  const sameAccountBatch = () => {
    const sameAccount = targets.filter((c) => c.latest.accountId === convo.latest.accountId);
    const skipped = targets.length - sameAccount.length;
    return {
      ids: sameAccount.flatMap((c) => c.messages.map((m) => m.id)),
      skipNotice: skipped > 0 ? t(locale, 'mail.skippedOtherAccounts', { count: skipped }) : null,
    };
  };
  const runWithSkipNotice = (p: Promise<{ error: string | null }>, skipNotice: string | null) =>
    void p.then((r) => report(r.error ?? skipNotice));

  const snoozeOptions: Array<{ key: string; until: Date }> = [
    { key: 'mail.laterToday', until: addHours(today, 4) },
    { key: 'mail.tomorrow', until: addDays(today, 1) },
    { key: 'mail.thisWeekend', until: nextSaturday(today) },
    { key: 'mail.nextWeek', until: addDays(today, 7) },
  ];

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
      <ContextMenuContent className="w-56">
        {latest.isDraft ? (
          <ContextMenuItem onSelect={() => void editDraftFromList(latest.id).then(report)}>
            <PenSquare />
            {t(locale, 'mail.editDraft')}
          </ContextMenuItem>
        ) : (
          <>
            <ContextMenuItem onSelect={() => void replyFromList(latest.id, false).then(report)}>
              <Reply />
              {t(locale, 'mail.reply')}
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => void replyFromList(latest.id, true).then(report)}>
              <ReplyAll />
              {t(locale, 'mail.replyAll')}
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => void forwardFromList(latest.id).then(report)}>
              <Forward />
              {t(locale, 'mail.forward')}
            </ContextMenuItem>
          </>
        )}
        <ContextMenuSeparator />
        <ContextMenuItem onSelect={() => runRemoving(actOnMessages(targetIds, 'archive'))}>
          <Archive />
          {t(locale, 'mail.archive')}
        </ContextMenuItem>
        {inSpamFolder ? (
          <ContextMenuItem onSelect={() => runRemoving(actOnMessages(targetIds, 'notSpam'))}>
            <Inbox />
            {t(locale, 'mail.notSpam')}
          </ContextMenuItem>
        ) : (
          <ContextMenuItem onSelect={() => runRemoving(actOnMessages(targetIds, 'spam'))}>
            <ArchiveX />
            {t(locale, 'mail.moveToJunk')}
          </ContextMenuItem>
        )}
        <ContextMenuItem
          variant="destructive"
          onSelect={() => {
            void (async () => {
              if (!(await confirmMoveToTrash(locale, targetIds.length))) return;
              runRemoving(actOnMessages(targetIds, 'trash'));
            })();
          }}
        >
          <Trash2 />
          {t(locale, 'mail.moveToTrash')}
        </ContextMenuItem>
        <FolderPickerSub
          convo={convo}
          labelKey="mail.moveToFolder"
          icon={<FolderInput />}
          onPick={(folderId) => {
            const { ids, skipNotice } = sameAccountBatch();
            clearSelection();
            runWithSkipNotice(moveMessages(ids, folderId), skipNotice);
          }}
        />
        <FolderPickerSub
          convo={convo}
          labelKey="mail.copyToFolder"
          icon={<Copy />}
          onPick={(folderId) => {
            const { ids, skipNotice } = sameAccountBatch();
            runWithSkipNotice(copyMessages(ids, folderId), skipNotice);
          }}
        />
        <ContextMenuSeparator />
        {anyUnread ? (
          <ContextMenuItem onSelect={() => run(patchMessages(targetIds, { isRead: true }))}>
            <MailOpen />
            {t(locale, 'mail.markRead')}
          </ContextMenuItem>
        ) : (
          <ContextMenuItem onSelect={() => run(patchMessages(targetIds, { isRead: false }))}>
            <Mail />
            {t(locale, 'mail.markUnread')}
          </ContextMenuItem>
        )}
        <ContextMenuItem
          onSelect={() => run(patchMessages(targetIds, { isStarred: anyUnstarred }))}
        >
          {anyUnstarred ? <Star /> : <StarOff />}
          {t(locale, anyUnstarred ? 'mail.star' : 'mail.unstar')}
        </ContextMenuItem>
        <ContextMenuItem
          onSelect={() => {
            const ui = useUIStore.getState();
            for (const c of targets) {
              const threadId =
                c.latest.threadId ?? c.messages.find((m) => m.threadId)?.threadId ?? null;
              const messageIds = c.messages.map((m) => m.id);
              if (notifyMuted) {
                // Unmute: banners resume and the messages return to the list.
                if (threadId) setThreadMuted(threadId, false);
                for (const id of messageIds) {
                  if (ui.mutedMessageIds.includes(id)) ui.toggleMuteMessage(id);
                }
              } else {
                // Mute: hide from the list (session-local) and stop new-mail
                // banners for this thread (persisted in notification prefs).
                if (threadId) setThreadMuted(threadId, true);
                for (const id of messageIds) {
                  if (!ui.mutedMessageIds.includes(id)) ui.toggleMuteMessage(id);
                }
                if (ui.selectedMessageId && messageIds.includes(ui.selectedMessageId)) {
                  ui.setSelectedMessage(null);
                }
              }
            }
          }}
        >
          {notifyMuted ? <Bell /> : <BellOff />}
          {t(locale, notifyMuted ? 'mail.unmuteThread' : 'mail.muteThread')}
        </ContextMenuItem>
        <ContextMenuSub>
          <ContextMenuSubTrigger>
            <Clock />
            {t(locale, 'mail.snooze')}
          </ContextMenuSubTrigger>
          <ContextMenuSubContent className="w-48">
            {snoozeOptions.map((opt) => (
              <ContextMenuItem
                key={opt.key}
                onSelect={() => runRemoving(snoozeMessages(targetIds, opt.until))}
              >
                {t(locale, opt.key)}
                <span className="ml-auto text-xs text-muted-foreground">
                  {format(opt.until, 'h:mm a')}
                </span>
              </ContextMenuItem>
            ))}
          </ContextMenuSubContent>
        </ContextMenuSub>
      </ContextMenuContent>
    </ContextMenu>
  );
}
