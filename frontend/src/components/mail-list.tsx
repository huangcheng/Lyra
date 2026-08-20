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
      <div className="mail-list-avatar">{getInitials(message.from.name ?? message.from.email)}</div>
      <div className="mail-list-content">
        <div className="mail-list-top">
          <span className="mail-list-sender">{message.from.name ?? message.from.email}</span>
          <span className="mail-list-date">{formatMailDate(message.date)}</span>
        </div>
        <div className="mail-list-subject">{message.subject}</div>
        <div className="mail-list-snippet">{message.snippet}</div>
      </div>
      {message.isStarred && <span className="mail-list-star">⭐</span>}
    </button>
  );
}

export function MailList() {
  const locale = useUIStore((s) => s.locale);
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const searchQuery = useUIStore((s) => s.searchQuery);
  const setSearchQuery = useUIStore((s) => s.setSearchQuery);
  const messages = useMailStore((s) => s.getMessagesForFolder(selectedFolderId ?? ''));
  const token = useAuthStore((s) => s.token);
  const upsertMessage = useMailStore((s) => s.upsertMessage);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // Filter by search query
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
        <input
          type="text"
          className="mail-list-search"
          placeholder={t(locale, 'mail.searchPlaceholder')}
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
        />
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
