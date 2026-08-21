/**
 * Middle pane — message list for the selected folder.
 *
 * Modeled after the shadcn mail example message list.
 * Includes loading, empty, and error states.
 */

import { useState, useEffect } from 'react';
import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { t } from '../i18n';
import { formatMailDate, getInitials, cn } from '../lib/utils';
import type { MailMessage } from '../types';

function ReplyGlyph() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden>
      <polyline
        points="9 17 4 12 9 7"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M20 18v-2a4 4 0 0 0-4-4H4"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function MessageItem({ message }: { message: MailMessage }) {
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const isSelected = selectedMessageId === message.id;

  return (
    <button
      type="button"
      className={cn(
        'mail-list-item',
        isSelected && 'mail-list-item--selected',
        !message.isRead && 'mail-list-item--unread',
      )}
      onClick={() => setSelectedMessage(message.id)}
    >
      <span className="mail-list-status" aria-hidden>
        {message.isReplied ? <ReplyGlyph /> : null}
      </span>
      <div className="mail-list-avatar">{getInitials(message.from.name ?? message.from.email)}</div>
      <div className="mail-list-content">
        <div className="mail-list-top">
          <span className="mail-list-lead">
            {!message.isRead && <span className="mail-list-unread-dot" />}
            <span className="mail-list-sender">{message.from.name ?? message.from.email}</span>
          </span>
          <span className="mail-list-date">{formatMailDate(message.date)}</span>
        </div>
        <div className="mail-list-subject">{message.subject}</div>
        <div className="mail-list-snippet">{message.snippet}</div>
      </div>
    </button>
  );
}

export function MailList() {
  const locale = useUIStore((s) => s.locale);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const setSearchQuery = useUIStore((s) => s.setSearchQuery);
  const searchOpen = useUIStore((s) => s.searchOpen);
  const setSearchOpen = useUIStore((s) => s.setSearchOpen);
  const folders = useMailStore((s) => s.folders);
  const messages = useMailStore((s) => s.getMessagesForFolder(selectedFolderId ?? ''));
  const token = useAuthStore((s) => s.token);
  const upsertMessage = useMailStore((s) => s.upsertMessage);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchHits, setSearchHits] = useState<MailMessage[] | null>(null);
  const [searching, setSearching] = useState(false);

  const folder = selectedFolderId ? folders[selectedFolderId] : undefined;
  const folderTitle = folder
    ? folder.role
      ? t(locale, `nav.${folder.role}`)
      : folder.name
    : t(locale, 'nav.inbox');

  const mapApiMessage = (msg: Record<string, unknown>): MailMessage => {
    const parseAddr = (json?: unknown) => {
      if (typeof json !== 'string') return { email: 'unknown' };
      try {
        const parsed = JSON.parse(json) as { raw?: string };
        if (parsed.raw) {
          const match = parsed.raw.match(/^(.+?)\s*<(.+?)>$/);
          if (match) return { name: match[1].trim(), email: match[2].trim() };
          return { email: parsed.raw };
        }
      } catch {
        /* fall through */
      }
      return { email: 'unknown' };
    };

    const parseAddrs = (json?: unknown) => {
      if (typeof json !== 'string') return [];
      try {
        const parsed = JSON.parse(json) as unknown;
        if (Array.isArray(parsed)) {
          return parsed.map((item: string) => {
            const match = item.match(/^(.+?)\s*<(.+?)>$/);
            if (match) return { name: match[1].trim(), email: match[2].trim() };
            return { email: item };
          });
        }
      } catch {
        /* fall through */
      }
      return [];
    };

    return {
      id: String(msg.id),
      accountId: String(msg.accountId),
      folderId: String(msg.folderId),
      subject: (msg.subject as string) ?? '(no subject)',
      from: parseAddr(msg.fromAddress),
      to: parseAddrs(msg.toAddresses),
      cc: parseAddrs(msg.ccAddresses),
      date: (msg.date as string) ?? new Date().toISOString(),
      snippet: (msg.snippet as string) ?? '',
      bodyText: msg.bodyText as string | undefined,
      bodyHtml: msg.bodyHtml as string | undefined,
      isRead: Boolean(msg.isRead),
      isStarred: Boolean(msg.isStarred),
      isDraft: false,
      hasAttachments: Boolean(msg.hasAttachments),
    };
  };

  // Fetch messages when folder changes
  useEffect(() => {
    if (!selectedFolderId || !token) return;

    const fetchMessages = async () => {
      setLoading(true);
      setError(null);
      try {
        const res = await fetch(`/api/folders/${selectedFolderId}/messages`, {
          headers: { Authorization: `Bearer ${token}` },
        });
        if (!res.ok) throw new Error('Failed to fetch messages');
        const data = (await res.json()) as Record<string, unknown>[];
        for (const msg of data) {
          upsertMessage(mapApiMessage(msg));
        }
      } catch (err: unknown) {
        setError(err instanceof Error ? err.message : 'Failed to load messages');
      } finally {
        setLoading(false);
      }
    };

    void fetchMessages();
  }, [selectedFolderId, token, upsertMessage]);

  // Server search (local DB index) when query is 2+ chars
  useEffect(() => {
    const q = searchQuery.trim();
    if (!token || q.length < 2) {
      setSearchHits(null);
      setSearching(false);
      return;
    }

    const handle = window.setTimeout(() => {
      void (async () => {
        setSearching(true);
        try {
          const params = new URLSearchParams({ q });
          if (selectedAccountId) params.set('accountId', selectedAccountId);
          const res = await fetch(`/api/messages/search?${params}`, {
            headers: { Authorization: `Bearer ${token}` },
          });
          if (!res.ok) throw new Error('Search failed');
          const data = (await res.json()) as Record<string, unknown>[];
          const mapped = data.map(mapApiMessage);
          for (const msg of mapped) upsertMessage(msg);
          setSearchHits(mapped);
        } catch {
          setSearchHits([]);
        } finally {
          setSearching(false);
        }
      })();
    }, 280);

    return () => window.clearTimeout(handle);
  }, [searchQuery, token, selectedAccountId, upsertMessage]);

  const filtered =
    searchHits ??
    (searchQuery.trim().length > 0 && searchQuery.trim().length < 2
      ? messages.filter(
          (m) =>
            m.subject.toLowerCase().includes(searchQuery.toLowerCase()) ||
            m.from.email.toLowerCase().includes(searchQuery.toLowerCase()) ||
            m.snippet.toLowerCase().includes(searchQuery.toLowerCase()),
        )
      : messages);

  const listTitle =
    searchHits !== null ? t(locale, 'mail.searchPlaceholder') : folderTitle;

  return (
    <div className="mail-list">
      <div className="mail-list-header">
        <div className="mail-list-header-row">
          <h2 className="mail-list-title">
            {listTitle}
            <span className="mail-list-count"> · {filtered.length}</span>
          </h2>
          <button
            type="button"
            className="mail-list-search-toggle"
            aria-label={t(locale, 'mail.searchPlaceholder')}
            aria-pressed={searchOpen}
            onClick={() => setSearchOpen(!searchOpen)}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden>
              <circle cx="11" cy="11" r="7" stroke="currentColor" strokeWidth="1.75" />
              <path
                d="M20 20l-3-3"
                stroke="currentColor"
                strokeWidth="1.75"
                strokeLinecap="round"
              />
            </svg>
          </button>
        </div>
        {searchOpen && (
          <input
            type="text"
            className="mail-list-search"
            placeholder={t(locale, 'mail.searchPlaceholder')}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            autoFocus
          />
        )}
      </div>
      <div className="mail-list-body">
        {loading || searching ? (
          <div className="mail-list-loading">
            <span className="loading-spinner" />
            <span>{t(locale, 'common.loading')}</span>
          </div>
        ) : error ? (
          <div className="mail-list-error">
            <span className="error-icon">⚠️</span>
            <span>{error}</span>
          </div>
        ) : filtered.length === 0 ? (
          <div className="mail-list-empty">{t(locale, 'mail.noMessages')}</div>
        ) : (
          filtered.map((msg) => <MessageItem key={msg.id} message={msg} />)
        )}
      </div>
    </div>
  );
}
