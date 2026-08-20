/**
 * Middle pane — message list for the selected folder.
 *
 * Modeled after the shadcn mail example message list.
 */

import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
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
        {filtered.length === 0 ? (
          <div className="mail-list-empty">{t(locale, 'mail.noMessages')}</div>
        ) : (
          filtered.map((msg) => <MessageItem key={msg.id} message={msg} />)
        )}
      </div>
    </div>
  );
}
