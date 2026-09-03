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
  BellOff,
  Check,
  Clock,
  Copy,
  FolderInput,
  Forward,
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
import { t, type SupportedLocale } from '@/i18n';
import { confirmMoveToTrash } from '@/lib/confirm-trash';
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
import { buildAccountMoveFolderEntries, type MoveFolderEntry } from '@/lib/folder-tree';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

function folderPickerLabel(entry: MoveFolderEntry, locale: SupportedLocale): string {
  return entry.role ? t(locale, `mail.folder.${entry.role}`) : entry.name;
}

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
          const label = folderPickerLabel(e, locale).toLowerCase();
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
                <span className="truncate">{folderPickerLabel(f, locale)}</span>
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
  onActionError,
  children,
}: {
  convo: Conversation;
  /** Surface a failure in the list's error line (null clears it). */
  onActionError: (message: string | null) => void;
  children: ReactNode;
}) {
  const locale = useUIStore((s) => s.locale);
  const latest = convo.latest;
  const ids = convo.messages.map((m) => m.id);
  const today = new Date();

  const report = (error: string | null) => onActionError(error);
  const run = (p: Promise<{ error: string | null }>) => void p.then((r) => report(r.error));

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
        <ContextMenuItem onSelect={() => run(actOnMessages(ids, 'archive'))}>
          <Archive />
          {t(locale, 'mail.archive')}
        </ContextMenuItem>
        <ContextMenuItem onSelect={() => run(actOnMessages(ids, 'spam'))}>
          <ArchiveX />
          {t(locale, 'mail.moveToJunk')}
        </ContextMenuItem>
        <ContextMenuItem
          variant="destructive"
          onSelect={() => {
            void (async () => {
              if (!(await confirmMoveToTrash(locale, ids.length))) return;
              run(actOnMessages(ids, 'trash'));
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
          onPick={(folderId) => run(moveMessages(ids, folderId))}
        />
        <FolderPickerSub
          convo={convo}
          labelKey="mail.copyToFolder"
          icon={<Copy />}
          onPick={(folderId) => run(copyMessages(ids, folderId))}
        />
        <ContextMenuSeparator />
        {convo.unreadCount > 0 ? (
          <ContextMenuItem onSelect={() => run(patchMessages(ids, { isRead: true }))}>
            <MailOpen />
            {t(locale, 'mail.markRead')}
          </ContextMenuItem>
        ) : (
          <ContextMenuItem onSelect={() => run(patchMessages(ids, { isRead: false }))}>
            <Mail />
            {t(locale, 'mail.markUnread')}
          </ContextMenuItem>
        )}
        <ContextMenuItem onSelect={() => run(patchMessages(ids, { isStarred: !convo.anyStarred }))}>
          {convo.anyStarred ? <StarOff /> : <Star />}
          {t(locale, convo.anyStarred ? 'mail.unstar' : 'mail.star')}
        </ContextMenuItem>
        <ContextMenuItem
          onSelect={() => {
            // Session-local mute, same store the reader's overflow menu uses.
            const ui = useUIStore.getState();
            for (const id of ids) {
              if (!ui.mutedMessageIds.includes(id)) ui.toggleMuteMessage(id);
            }
            if (ui.selectedMessageId && ids.includes(ui.selectedMessageId)) {
              ui.setSelectedMessage(null);
            }
          }}
        >
          <BellOff />
          {t(locale, 'mail.muteThread')}
        </ContextMenuItem>
        <ContextMenuSub>
          <ContextMenuSubTrigger>
            <Clock />
            {t(locale, 'mail.snooze')}
          </ContextMenuSubTrigger>
          <ContextMenuSubContent className="w-48">
            {snoozeOptions.map((opt) => (
              <ContextMenuItem key={opt.key} onSelect={() => run(snoozeMessages(ids, opt.until))}>
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
