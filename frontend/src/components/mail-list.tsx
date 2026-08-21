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

  const folder = selectedFolderId ? folders[selectedFolderId] : undefined;
  const folderTitle = folder
    ? folder.role
      ? t(locale, `nav.${folder.role}`)
      : folder.name
    : t(locale, 'nav.inbox');

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
        const data = await res.json();

        for (const msg of data) {
          const parseAddr = (json?: string) => {
            if (!json) return { email: 'unknown' };
            try {
              const parsed = JSON.parse(json);
              if (parsed.raw) {
                const match = parsed.raw.match(/^(.+?)\s*<(.+?)>$/);
                if (match) return { name: match[1].trim(), email: match[2].trim() };
                return { email: parsed.raw };
              }
              return { email: 'unknown' };
            } catch {
              return { email: 'unknown' };
            }
          };

          const parseAddrs = (json?: string) => {
            if (!json) return [];
            try {
              const parsed = JSON.parse(json);
              if (Array.isArray(parsed)) {
                return parsed.map((item: string) => {
                  const match = item.match(/^(.+?)\s*<(.+?)>$/);
                  if (match) return { name: match[1].trim(), email: match[2].trim() };
                  return { email: item };
                });
              }
              return [];
            } catch {
              return [];
            }
          };

          upsertMessage({
            id: msg.id,
            accountId: msg.accountId,
            folderId: msg.folderId,
            subject: msg.subject ?? '(no subject)',
            from: parseAddr(msg.fromAddress),
            to: parseAddrs(msg.toAddresses),
            cc: parseAddrs(msg.ccAddresses),
            date: msg.date ?? new Date().toISOString(),
            snippet: msg.snippet ?? '',
            bodyText: msg.bodyText,
            bodyHtml: msg.bodyHtml,
            isRead: msg.isRead,
            isStarred: msg.isStarred,
            isDraft: false,
            hasAttachments: msg.hasAttachments,
          });
        }
      } catch (err: unknown) {
        setError(err instanceof Error ? err.message : 'Failed to load messages');
      } finally {
        setLoading(false);
      }
    };

    fetchMessages();
  }, [selectedFolderId, token, upsertMessage]);

  const filtered = searchQuery
    ? messages.filter(
        (m) =>
          m.subject.toLowerCase().includes(searchQuery.toLowerCase()) ||
          m.from.email.toLowerCase().includes(searchQuery.toLowerCase()) ||
          m.snippet.toLowerCase().includes(searchQuery.toLowerCase()),
      )
    : messages;

  return (
    <div className="mail-list">
      <div className="mail-list-header">
        <div className="mail-list-header-row">
          <h2 className="mail-list-title">
            {folderTitle}
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
        {loading ? (
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
