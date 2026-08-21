/**
 * Three-pane mail layout with resizable splitters.
 */

import {
  useCallback,
  useEffect,
  useRef,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { Sidebar } from './sidebar';
import { MailList } from './mail-list';
import { MailView } from './mail-view';
import { ComposeDialog } from './compose-dialog';
import {
  useUIStore,
  DEFAULT_SIDEBAR_WIDTH,
  DEFAULT_LIST_WIDTH,
} from '../stores/ui';
import { useMailData } from '../lib/use-mail-data';
import { cn } from '../lib/utils';

type DragTarget = 'sidebar' | 'list' | null;

export function MailLayout() {
  const sidebarCollapsed = useUIStore((s) => s.sidebarCollapsed);
  const sidebarWidth = useUIStore((s) => s.sidebarWidth);
  const listWidth = useUIStore((s) => s.listWidth);
  const setPaneWidths = useUIStore((s) => s.setPaneWidths);
  const resetPaneWidths = useUIStore((s) => s.resetPaneWidths);

  useMailData();

  const dragRef = useRef<{
    target: DragTarget;
    startX: number;
    startSidebar: number;
    startList: number;
  }>({
    target: null,
    startX: 0,
    startSidebar: DEFAULT_SIDEBAR_WIDTH,
    startList: DEFAULT_LIST_WIDTH,
  });

  const onPointerMove = useCallback(
    (e: PointerEvent) => {
      const drag = dragRef.current;
      if (!drag.target) return;
      const dx = e.clientX - drag.startX;
      if (drag.target === 'sidebar') {
        setPaneWidths({ sidebarWidth: drag.startSidebar + dx });
      } else {
        setPaneWidths({ listWidth: drag.startList + dx });
      }
    },
    [setPaneWidths],
  );

  const onPointerUp = useCallback(() => {
    dragRef.current.target = null;
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
  }, []);

  useEffect(() => {
    window.addEventListener('pointermove', onPointerMove);
    window.addEventListener('pointerup', onPointerUp);
    return () => {
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('pointerup', onPointerUp);
    };
  }, [onPointerMove, onPointerUp]);

  const startDrag = (target: 'sidebar' | 'list') => (e: ReactPointerEvent) => {
    e.preventDefault();
    dragRef.current = {
      target,
      startX: e.clientX,
      startSidebar: sidebarWidth,
      startList: listWidth,
    };
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  };

  const effectiveSidebar = sidebarCollapsed ? 0 : sidebarWidth;

  return (
    <div
      className="mail-layout"
      style={
        {
          '--sidebar-width': `${effectiveSidebar}px`,
          '--mail-list-width': `${listWidth}px`,
        } as CSSProperties
      }
    >
      <div
        className={cn('mail-layout-sidebar', sidebarCollapsed && 'mail-layout-sidebar--collapsed')}
      >
        <Sidebar />
      </div>
      <div
        className="mail-splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize sidebar"
        onPointerDown={startDrag('sidebar')}
        onDoubleClick={() => resetPaneWidths()}
      />
      <div className="mail-layout-list">
        <MailList />
      </div>
      <div
        className="mail-splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize list"
        onPointerDown={startDrag('list')}
        onDoubleClick={() => resetPaneWidths()}
      />
      <div className="mail-layout-view">
        <MailView />
      </div>
      <ComposeDialog />
    </div>
  );
}
