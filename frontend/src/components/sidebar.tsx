/**
 * Left sidebar — folder list, account switcher, language toggle.
 *
 * Modeled after the shadcn mail example sidebar.
 */

import { useState } from 'react';
import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';
import { t } from '../i18n';
import type { MailFolder } from '../types';

/** Icon placeholder — swap for lucide-react or similar later. */
function FolderIcon({ role }: { role?: string }) {
  const icons: Record<string, string> = {
    inbox: '📥',
    sent: '📤',
    drafts: '📝',
    trash: '🗑️',
    spam: '⚠️',
    archive: '📦',
  };
  return <span className="sidebar-icon">{icons[role ?? ''] ?? '📁'}</span>;
}

function FolderItem({ folder }: { folder: MailFolder }) {
  const selectedFolderId = useUIStore((s) => s.selectedFolderId);
  const setSelectedFolder = useUIStore((s) => s.setSelectedFolder);
  const locale = useUIStore((s) => s.locale);
  const isSelected = selectedFolderId === folder.id;
  const label = folder.role ? t(locale, `nav.${folder.role}`) : folder.name;

  return (
    <button
      type="button"
      className={`sidebar-folder ${isSelected ? 'sidebar-folder--selected' : ''}`}
      onClick={() => setSelectedFolder(folder.id)}
    >
      <FolderIcon role={folder.role} />
      <span className="sidebar-folder-name">{label}</span>
      {folder.unreadCount > 0 && <span className="sidebar-folder-badge">{folder.unreadCount}</span>}
    </button>
  );
}

export function Sidebar() {
  const locale = useUIStore((s) => s.locale);
  const selectedAccountId = useUIStore((s) => s.selectedAccountId);
  const folders = useMailStore((s) => s.getFoldersForAccount(selectedAccountId ?? ''));
  const accounts = useMailStore((s) => s.accounts);
  const setLocale = useUIStore((s) => s.setLocale);
  const setSelectedAccount = useUIStore((s) => s.setSelectedAccount);
  const setComposeOpen = useUIStore((s) => s.setComposeOpen);
  const clearSession = useAuthStore((s) => s.clearSession);
  const user = useAuthStore((s) => s.user);
  const token = useAuthStore((s) => s.token);

  const [syncing, setSyncing] = useState(false);

  const handleLogout = () => {
    const token = useAuthStore.getState().token;
    if (token) {
      fetch('/api/auth/logout', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
      }).catch(() => {});
    }
    localStorage.removeItem('lyra_token');
    clearSession();
  };

  const handleNavigate = (path: string) => {
    window.location.href = path;
  };

  const handleSync = async () => {
    const accountId = selectedAccountId ?? accounts[0]?.id;
    if (!accountId || !token) return;
    setSyncing(true);
    try {
      await fetch(`/api/accounts/${accountId}/sync`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
      });
      // Reload data after sync
      window.dispatchEvent(new CustomEvent('lyra:sync-complete'));
    } catch {
      // sync error handled by event stream
    } finally {
      setSyncing(false);
    }
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <h1 className="sidebar-title">{t(locale, 'app.name')}</h1>
        <div className="sidebar-header-actions">
          <button
            type="button"
            className="sidebar-compose-btn"
            onClick={() => setComposeOpen(true)}
            title={t(locale, 'mail.compose')}
          >
            ✏️ {t(locale, 'mail.compose')}
          </button>
          <button
            type="button"
            className="sidebar-sync-btn"
            onClick={handleSync}
            disabled={syncing}
            title={syncing ? t(locale, 'mail.syncing') : t(locale, 'mail.syncNow')}
          >
            {syncing ? '⏳' : '🔄'}{' '}
            {syncing ? t(locale, 'mail.syncing') : t(locale, 'mail.syncNow')}
          </button>
        </div>
      </div>

      {/* Account selector */}
      {accounts.length > 1 && (
        <select
          className="sidebar-account-select"
          value={selectedAccountId ?? ''}
          onChange={(e) => setSelectedAccount(e.target.value || null)}
        >
          {accounts.map((a) => (
            <option key={a.id} value={a.id}>
              {a.displayName || a.emailAddress}
            </option>
          ))}
        </select>
      )}

      {/* Folder list */}
      <nav className="sidebar-folders">
        {folders.map((folder) => (
          <FolderItem key={folder.id} folder={folder} />
        ))}
      </nav>

      {/* Navigation links */}
      <nav className="sidebar-nav">
        <button
          type="button"
          className="sidebar-nav-item"
          onClick={() => handleNavigate('/contacts')}
        >
          <span className="sidebar-icon">👤</span>
          <span>{t(locale, 'nav.contacts')}</span>
        </button>
        <button
          type="button"
          className="sidebar-nav-item"
          onClick={() => handleNavigate('/calendar')}
        >
          <span className="sidebar-icon">📅</span>
          <span>{t(locale, 'nav.calendar')}</span>
        </button>
        <button
          type="button"
          className="sidebar-nav-item"
          onClick={() => handleNavigate('/settings')}
        >
          <span className="sidebar-icon">⚙️</span>
          <span>{t(locale, 'nav.settings')}</span>
        </button>
      </nav>

      {/* Footer: user info + logout + language */}
      <div className="sidebar-footer">
        {user && (
          <div className="sidebar-user">
            <span className="sidebar-username">{user.displayName || user.username}</span>
            <button
              type="button"
              className="sidebar-logout"
              onClick={handleLogout}
              title={t(locale, 'auth.logout')}
            >
              {t(locale, 'auth.logout')}
            </button>
          </div>
        )}
        <div className="language-switch">
          <button
            type="button"
            className={`lang-btn ${locale === 'en' ? 'lang-btn--active' : ''}`}
            onClick={() => setLocale('en')}
          >
            EN
          </button>
          <button
            type="button"
            className={`lang-btn ${locale === 'zh' ? 'lang-btn--active' : ''}`}
            onClick={() => setLocale('zh')}
          >
            中文
          </button>
        </div>
      </div>
    </aside>
  );
}
