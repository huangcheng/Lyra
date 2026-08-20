/**
 * Three-pane mail layout chrome.
 *
 * Follows the shadcn mail example structure:
 *   [sidebar] [mail-list] [mail-view]
 *
 * The sidebar is collapsible; the list and view panes are resizable
 * (resizability is a v2 enhancement — this v1 shell uses fixed proportions).
 */

import { Sidebar } from './sidebar';
import { MailList } from './mail-list';
import { MailView } from './mail-view';
import { useUIStore } from '../stores/ui';
import { cn } from '../lib/utils';

export function MailLayout() {
  const sidebarCollapsed = useUIStore((s) => s.sidebarCollapsed);

  return (
    <div className="mail-layout">
      <div
        className={cn('mail-layout-sidebar', sidebarCollapsed && 'mail-layout-sidebar--collapsed')}
      >
        <Sidebar />
      </div>
      <div className="mail-layout-list">
        <MailList />
      </div>
      <div className="mail-layout-view">
        <MailView />
      </div>
    </div>
  );
}
