/**
 * Left sidebar — folder list, account switcher, language toggle.
 *
 * Modeled after the shadcn mail example sidebar.
 */

import { useMailStore } from '../stores/mail';
import { useUIStore } from '../stores/ui';
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

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <h1 className="sidebar-title">{t(locale, 'app.name')}</h1>
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

      {/* Language switch */}
      <div className="sidebar-footer">
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
