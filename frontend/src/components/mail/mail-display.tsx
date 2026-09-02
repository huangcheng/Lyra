/**
 * Reading pane — conversation stack.
 *
 * Messages that share an account + normalized subject (Re:/Fwd: stripped)
 * render as Fastmail-style stacked cards: oldest first, unread and the
 * latest expanded, the rest collapsed to a one-line header. Toolbar
 * actions act on the selected message; reply/forward open the composer.
 */

import { addDays, addHours, format, nextSaturday } from 'date-fns';
import {
  Archive,
  ArchiveX,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock,
  FolderInput,
  Forward,
  MailOpen,
  MoreVertical,
  Reply,
  ReplyAll,
  Trash2,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/empty-state';
import { MessageCard } from '@/components/mail/message-card';

import { Button } from '@/components/ui/button';
import { Calendar } from '@/components/ui/calendar';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Separator } from '@/components/ui/separator';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import {
  baseSubject,
  conversationKeyOf,
  conversationMembers,
  groupIntoConversations,
} from '@/lib/conversation';
import { MARK_READ_OPEN_DWELL_MS } from '@/lib/mark-read-policy';
import { markMessageReadOnServer } from '@/lib/mark-message-read';
import { buildForwardDraft, buildReplyDraft } from '@/lib/compose-draft';
import { textToHtml } from '@/lib/compose-html';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { useMediaQuery } from '@/lib/use-media-query';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

const SCROLL_END_THRESHOLD_PX = 32;

function isScrolledToBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= SCROLL_END_THRESHOLD_PX;
}

export function MailDisplay() {
  const locale = useUIStore((s) => s.locale);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const openCompose = useUIStore((s) => s.openCompose);
  const mutedMessageIds = useUIStore((s) => s.mutedMessageIds);
  const toggleMuteMessage = useUIStore((s) => s.toggleMuteMessage);
  const token = useAuthStore((s) => s.token);
  const isMobile = useMediaQuery('(max-width: 1023px)');
  const mail = useMailStore((s) => (selectedMessageId ? s.messages[selectedMessageId] : null));
  const messages = useMailStore((s) => s.messages);
  const folders = useMailStore((s) => s.folders);
  const accounts = useMailStore((s) => s.accounts);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);
  const toggleStar = useMailStore((s) => s.toggleStar);
  const removeMessage = useMailStore((s) => s.removeMessage);

  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const bodyScrollRef = useRef<HTMLDivElement>(null);
  const autoMarkedIdRef = useRef<string | null>(null);
  const today = new Date();

  // Selected conversation's position in the current list view — drives the
  // ‹ n / total › pager in the toolbar.
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedFolderRole = useUIStore((s) => s.selectedFolderRole);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const listTab = useUIStore((s) => s.listTab);
  const mutedMessageIdsList = useUIStore((s) => s.mutedMessageIds);
  const getMessagesForView = useMailStore((s) => s.getMessagesForView);

  const viewConversations = useMemo(() => {
    const items = getMessagesForView(
      {
        accountId: selectedAccountId,
        folderId: selectedFolderId,
        folderRole: selectedFolderRole,
      },
      { messages, folders },
    );
    const filtered = (listTab === 'unread' ? items.filter((m) => !m.isRead) : items).filter(
      (m) => !mutedMessageIdsList.includes(m.id),
    );
    return groupIntoConversations(filtered);
  }, [
    getMessagesForView,
    messages,
    folders,
    selectedAccountId,
    selectedFolderId,
    selectedFolderRole,
    listTab,
    mutedMessageIdsList,
  ]);

  const convoPosition = useMemo(() => {
    if (!mail) return { index: -1, total: viewConversations.length };
    const idx = viewConversations.findIndex((c) => c.messages.some((m) => m.id === mail.id));
    return { index: idx, total: viewConversations.length };
  }, [viewConversations, mail]);

  const stepConversation = (delta: 1 | -1) => {
    const next = viewConversations[convoPosition.index + delta];
    if (!next) return;
    const target = next.messages.find((m) => !m.isRead) ?? next.latest;
    setSelectedMessage(target.id);
  };

  const conversation = useMemo(
    () => (mail ? conversationMembers(mail, messages) : []),
    [mail, messages],
  );
  const conversationKey = mail ? conversationKeyOf(mail) : null;

  // Conversation partners may live in folders the current view never
  // loaded (e.g. the Inbox original while reading Sent). Subject search is
  // unreliable for long special-char subjects (FTS tokenization), so
  // prefetch the whole account listing once per account instead.
  const accountPrefetchRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!mail || !token) return;
    const accountId = mail.accountId;
    if (accountPrefetchRef.current.has(accountId)) return;
    accountPrefetchRef.current.add(accountId);
    const params = new URLSearchParams({ accountId });
    void api<ApiMessage[]>(`/messages?${params}`)
      .then((data) => {
        for (const raw of data) upsertMessage(mapApiMessage(raw));
      })
      .catch(() => {
        accountPrefetchRef.current.delete(accountId);
      });
  }, [mail, token, upsertMessage]);

  // Hide partners sitting in Trash unless the selected message is there.
  const visibleConversation = useMemo(() => {
    if (!mail) return [];
    const selectedInTrash = folders[mail.folderId]?.role === 'trash';
    if (selectedInTrash) return conversation;
    return conversation.filter((m) => folders[m.folderId]?.role !== 'trash');
  }, [mail, conversation, folders]);

  // Expand state: unread and the latest message open by default; the user
  // can toggle any card. Overrides reset when the conversation changes
  // (state adjusted during render — React's documented reset pattern).
  const [expandOverrides, setExpandOverrides] = useState<Record<string, boolean>>({});
  const [expandOverridesKey, setExpandOverridesKey] = useState<string | null>(conversationKey);
  if (conversationKey !== expandOverridesKey) {
    setExpandOverridesKey(conversationKey);
    setExpandOverrides({});
  }
  const latestId = visibleConversation.length
    ? visibleConversation[visibleConversation.length - 1].id
    : null;
  const isExpanded = useCallback(
    (id: string, isRead: boolean) => expandOverrides[id] ?? (!isRead || id === latestId),
    [expandOverrides, latestId],
  );

  const tryAutoMarkRead = useCallback(async () => {
    if (!selectedMessageId || !token || markReadPolicy === 'manual') return;
    if (autoMarkedIdRef.current === selectedMessageId) return;
    const ok = await markMessageReadOnServer(selectedMessageId);
    if (ok) autoMarkedIdRef.current = selectedMessageId;
  }, [selectedMessageId, token, markReadPolicy]);

  useEffect(() => {
    if (markReadPolicy !== 'on_open' || !mail || mail.isRead) return;
    const timer = window.setTimeout(() => {
      void tryAutoMarkRead();
    }, MARK_READ_OPEN_DWELL_MS);
    return () => window.clearTimeout(timer);
  }, [markReadPolicy, mail, tryAutoMarkRead]);

  useEffect(() => {
    if (markReadPolicy !== 'on_scroll_end' || !mail || mail.isRead) return;
    const el = bodyScrollRef.current;
    if (!el) return;

    const onScroll = () => {
      if (isScrolledToBottom(el)) void tryAutoMarkRead();
    };

    const raf = requestAnimationFrame(() => {
      if (isScrolledToBottom(el)) void tryAutoMarkRead();
    });

    el.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      cancelAnimationFrame(raf);
      el.removeEventListener('scroll', onScroll);
    };
  }, [markReadPolicy, mail, tryAutoMarkRead, visibleConversation.length]);

  const replyToMessage = (m: MailMessage, all: boolean) => {
    openCompose(buildReplyDraft(m, all, accounts));
  };

  const handleReply = (all = false) => {
    if (!mail) return;
    replyToMessage(mail, all);
  };

  /** Hover-panel reply on a specific card: select it, then compose. */
  const handleReplyFor = (id: string, all: boolean) => {
    const m = useMailStore.getState().messages[id];
    if (!m) return;
    setSelectedMessage(id);
    replyToMessage(m, all);
  };

  const forwardMessage = (m: MailMessage) => {
    openCompose(buildForwardDraft(m, accounts));
  };

  const handleForward = () => {
    if (!mail) return;
    forwardMessage(mail);
  };

  /** Hover-panel forward on a specific card: select it, then compose. */
  const handleForwardFor = (id: string) => {
    const m = useMailStore.getState().messages[id];
    if (!m) return;
    setSelectedMessage(id);
    forwardMessage(m);
  };

  /** Hover-panel trash on a specific card (selection cleared only if it was selected). */
  const handleTrashFor = async (id: string) => {
    if (!token || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${id}/trash`, { method: 'POST' });
      removeMessage(id);
      if (selectedMessageId === id) setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handleAction = async (action: 'trash' | 'archive' | 'spam') => {
    if (!token || !mail || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/${action}`, { method: 'POST' });
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handleEditDraft = () => {
    if (!mail) return;
    openCompose({
      mode: 'draft',
      accountId: mail.accountId,
      to: mail.to.map((a) => a.email).join(', '),
      cc: (mail.cc ?? []).map((a) => a.email).join(', '),
      subject: mail.subject ?? '',
      body: mail.bodyText ?? '',
      initialHtml: mail.bodyHtml ?? textToHtml(mail.bodyText ?? ''),
      draftMessageId: mail.id,
    });
  };

  const handleMoveToFolder = async (folderId: string) => {
    if (!token || !mail || busy || folderId === mail.folderId) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/move`, {
        method: 'POST',
        body: JSON.stringify({ folderId }),
      });
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handleSnooze = async (until: Date) => {
    if (!token || !mail || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/snooze`, {
        method: 'POST',
        body: JSON.stringify({ until: until.toISOString() }),
      });
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handlePatch = async (body: { isRead?: boolean; isStarred?: boolean }) => {
    if (!token || !mail) return;
    try {
      await api(`/messages/${mail.id}`, {
        method: 'PATCH',
        body: JSON.stringify(body),
      });
    } catch {
      return;
    }
    if (body.isRead === false) {
      upsertMessage({ ...mail, isRead: false });
    }
    if (body.isRead === true) {
      markMessageRead(mail.id);
    }
    if (body.isStarred !== undefined) {
      toggleStar(mail.id);
    }
  };

  const accountFolders = useMemo(
    () =>
      Object.values(folders)
        .filter((f) => f.accountId === mail?.accountId)
        .sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)),
    [folders, mail?.accountId],
  );

  const disabled = !mail || busy;
  const toolbarIconClass =
    'shrink-0 rounded-[7px] text-ter-foreground hover:bg-accent hover:text-foreground disabled:opacity-50';

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center overflow-x-auto p-2">
        <div className="flex items-center gap-1.5">
          {isMobile ? (
            <Button
              variant="ghost"
              size="icon"
              className={toolbarIconClass}
              onClick={() => setSelectedMessage(null)}
            >
              <ChevronLeft className="h-4 w-4" />
              <span className="sr-only">{t(locale, 'common.back')}</span>
            </Button>
          ) : null}
          {!isMobile && convoPosition.index >= 0 && searchQuery.trim().length < 2 ? (
            <>
              <div className="flex items-center gap-0.5 text-[11px] tabular-nums text-muted-foreground">
                <Button
                  variant="ghost"
                  size="icon"
                  className={toolbarIconClass}
                  disabled={convoPosition.index + 1 >= convoPosition.total}
                  onClick={() => stepConversation(1)}
                >
                  <ChevronLeft className="h-4 w-4" />
                  <span className="sr-only">{t(locale, 'mail.prevConversation')}</span>
                </Button>
                <span>
                  {convoPosition.index + 1} / {convoPosition.total}
                </span>
                <Button
                  variant="ghost"
                  size="icon"
                  className={toolbarIconClass}
                  disabled={convoPosition.index <= 0}
                  onClick={() => stepConversation(-1)}
                >
                  <ChevronRight className="h-4 w-4" />
                  <span className="sr-only">{t(locale, 'mail.nextConversation')}</span>
                </Button>
              </div>
              <Separator orientation="vertical" className="mx-1 h-6" />
            </>
          ) : null}
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('archive')}
              >
                <Archive className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.archive')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.archive')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('spam')}
              >
                <ArchiveX className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.moveToJunk')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.moveToJunk')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('trash')}
              >
                <Trash2 className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.moveToTrash')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.moveToTrash')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <Popover>
              <PopoverTrigger asChild>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className={toolbarIconClass}
                    disabled={disabled}
                  >
                    <FolderInput className="h-4 w-4" />
                    <span className="sr-only">{t(locale, 'mail.moveToFolder')}</span>
                  </Button>
                </TooltipTrigger>
              </PopoverTrigger>
              <TooltipContent>{t(locale, 'mail.moveToFolder')}</TooltipContent>
              <PopoverContent className="w-60 p-1" align="start">
                <div className="max-h-72 overflow-y-auto">
                  {accountFolders.length === 0 ? (
                    <p className="px-2 py-1.5 text-xs text-muted-foreground">
                      {t(locale, 'mail.noFolders')}
                    </p>
                  ) : (
                    accountFolders.map((f) => (
                      <button
                        key={f.id}
                        type="button"
                        disabled={busy || f.id === mail?.folderId}
                        className="flex w-full items-center justify-between gap-2 rounded-sm px-2 py-1.5 text-sm hover:bg-accent disabled:opacity-40"
                        onClick={() => void handleMoveToFolder(f.id)}
                      >
                        <span className="truncate">{f.name}</span>
                        {f.id === mail?.folderId ? <Check className="size-3.5 shrink-0" /> : null}
                      </button>
                    ))
                  )}
                </div>
              </PopoverContent>
            </Popover>
          </Tooltip>
          {mail?.isDraft ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className={toolbarIconClass}
                  disabled={disabled}
                  onClick={handleEditDraft}
                >
                  <Reply className="h-4 w-4" />
                  <span className="sr-only">{t(locale, 'mail.editDraft')}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t(locale, 'mail.editDraft')}</TooltipContent>
            </Tooltip>
          ) : null}
          <Separator orientation="vertical" className="mx-1 h-6" />
          <Tooltip>
            <Popover>
              <PopoverTrigger asChild>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className={toolbarIconClass}
                    disabled={disabled}
                  >
                    <Clock className="h-4 w-4" />
                    <span className="sr-only">{t(locale, 'mail.snooze')}</span>
                  </Button>
                </TooltipTrigger>
              </PopoverTrigger>
              <PopoverContent className="flex w-[535px] p-0">
                <div className="flex flex-col gap-2 border-r px-2 py-4">
                  <div className="px-4 text-sm font-medium">{t(locale, 'mail.snoozeUntil')}</div>
                  <div className="grid min-w-[250px] gap-1">
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addHours(today, 4))}
                    >
                      {t(locale, 'mail.laterToday')}{' '}
                      <span className="ml-auto text-muted-foreground">
                        {format(addHours(today, 4), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 1))}
                    >
                      {t(locale, 'mail.tomorrow')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 1), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(nextSaturday(today))}
                    >
                      {t(locale, 'mail.thisWeekend')}
                      <span className="ml-auto text-muted-foreground">
                        {format(nextSaturday(today), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 7))}
                    >
                      {t(locale, 'mail.nextWeek')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 7), 'h:mm a')}
                      </span>
                    </Button>
                  </div>
                </div>
                <div className="p-2">
                  <Calendar
                    mode="single"
                    onSelect={(date) => {
                      if (!date) return;
                      const until = new Date(date);
                      until.setHours(18, 0, 0, 0);
                      void handleSnooze(until);
                    }}
                  />
                </div>
              </PopoverContent>
            </Popover>
            <TooltipContent>{t(locale, 'mail.snooze')}</TooltipContent>
          </Tooltip>
        </div>
        <div className="ml-auto flex items-center gap-1.5">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => handleReply()}
              >
                <Reply className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.reply')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.reply')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => handleReply(true)}
              >
                <ReplyAll className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.replyAll')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.replyAll')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={handleForward}
              >
                <Forward className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.forward')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.forward')}</TooltipContent>
          </Tooltip>
        </div>
        <Separator orientation="vertical" className="mx-2 h-6" />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className={toolbarIconClass} disabled={disabled}>
              <MoreVertical className="h-4 w-4" />
              <span className="sr-only">{t(locale, 'mail.more')}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {mail && !mail.isRead ? (
              <DropdownMenuItem onClick={() => void handlePatch({ isRead: true })}>
                {t(locale, 'mail.markRead')}
              </DropdownMenuItem>
            ) : null}
            <DropdownMenuItem onClick={() => void handlePatch({ isRead: false })}>
              {t(locale, 'mail.markUnread')}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => void handlePatch({ isStarred: !mail?.isStarred })}>
              {t(locale, 'mail.starThread')}
            </DropdownMenuItem>
            <DropdownMenuItem disabled>{t(locale, 'mail.addLabel')}</DropdownMenuItem>
            <DropdownMenuItem
              onClick={() => {
                if (!mail) return;
                toggleMuteMessage(mail.id);
                if (!mutedMessageIds.includes(mail.id)) {
                  setSelectedMessage(null);
                }
              }}
            >
              {mail && mutedMessageIds.includes(mail.id)
                ? t(locale, 'mail.unmuteThread')
                : t(locale, 'mail.muteThread')}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <Separator />
      {actionError ? (
        <div className="border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          {actionError}
        </div>
      ) : null}
      {mail ? (
        <div className="flex min-h-0 flex-1 flex-col">
          <div ref={bodyScrollRef} className="min-h-0 flex-1 overflow-auto bg-secondary/60">
            <div className="w-full px-4 py-4">
              {visibleConversation.length > 1 ? (
                <div className="px-1.5 pb-3">
                  <h2 className="font-display text-lg font-medium">
                    {baseSubject(mail.subject) || mail.subject}
                  </h2>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {t(locale, 'mail.conversationCount', { count: visibleConversation.length })}
                  </p>
                </div>
              ) : null}
              <div className="space-y-3">
                {visibleConversation.map((member) => (
                  <MessageCard
                    key={member.id}
                    messageId={member.id}
                    expanded={isExpanded(member.id, member.isRead)}
                    hideSubject={visibleConversation.length > 1}
                    onToggle={() =>
                      setExpandOverrides((prev) => ({
                        ...prev,
                        [member.id]: !isExpanded(member.id, member.isRead),
                      }))
                    }
                    onReply={(all) => handleReplyFor(member.id, all)}
                    onForward={() => handleForwardFor(member.id)}
                    onTrash={() => void handleTrashFor(member.id)}
                  />
                ))}
              </div>
            </div>
          </div>
        </div>
      ) : (
        <EmptyState icon={MailOpen} title={t(locale, 'mail.selectMessage')} />
      )}
    </div>
  );
}
