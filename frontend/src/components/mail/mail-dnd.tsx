/**
 * Drag-and-drop context for the mail shell.
 *
 * One DndContext covers both panes: conversation rows (useDraggable in
 * mail-list) drop onto folder rows (useDroppable in sidebar-folders), and
 * the account sections stay sortable (SortableContext in sidebar-folders).
 * Drag kinds are told apart by `active.data.current?.type`.
 */

import {
  DndContext,
  DragOverlay,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { useEffect, useState, type ReactNode } from 'react';

import { Badge } from '@/components/ui/badge';
import { t } from '@/i18n';
import { moveId, orderAccounts } from '@/lib/account-order';
import { moveMessages, type ConversationDragData } from '@/lib/conversation-actions';
import type { StandardFolderRole } from '@/lib/mail-api';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Drop-target payload on a concrete folder row. */
export interface FolderDropData {
  type: 'folder';
  accountId: string;
  folderId: string;
}

/** Drop-target payload on a unified role row (resolved per drag account). */
export interface UnifiedRoleDropData {
  type: 'folder';
  unified: true;
  role: StandardFolderRole;
}

export function MailDndProvider({ children }: { children: ReactNode }) {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const accountOrder = useUIStore((s) => s.accountOrder);
  const setAccountOrder = useUIStore((s) => s.setAccountOrder);
  // 6px movement threshold: plain clicks on rows/folders still select.
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));

  const [drag, setDrag] = useState<ConversationDragData | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!error) return;
    const handle = window.setTimeout(() => setError(null), 6000);
    return () => window.clearTimeout(handle);
  }, [error]);

  const onDragStart = (event: DragStartEvent) => {
    const data = event.active.data.current as ConversationDragData | undefined;
    if (data?.type === 'conversation') setDrag(data);
  };

  const dropFolderId = (
    data: ConversationDragData,
    overData: FolderDropData | UnifiedRoleDropData | undefined,
  ): string | null => {
    if (!overData || overData.type !== 'folder') return null;
    if ('unified' in overData && overData.unified) {
      // Unified role row: target is the drag account's folder with that role.
      const folders = useMailStore.getState().folders;
      const target = Object.values(folders).find(
        (f) => f.accountId === data.accountId && f.role === overData.role,
      );
      return target?.id ?? null;
    }
    const folder = overData as FolderDropData;
    if (folder.accountId !== data.accountId) return null;
    return folder.folderId;
  };

  const handleConversationDrop = async (
    data: ConversationDragData,
    overData: FolderDropData | UnifiedRoleDropData | undefined,
  ) => {
    const targetFolderId = dropFolderId(data, overData);
    if (!targetFolderId) return;
    // Skip messages already sitting in the target (cross-folder copies).
    const messages = useMailStore.getState().messages;
    const ids = data.messageIds.filter((id) => messages[id]?.folderId !== targetFolderId);
    if (ids.length === 0) return;
    setProgress({ done: 0, total: ids.length });
    const res = await moveMessages(ids, targetFolderId, (done) =>
      setProgress({ done, total: ids.length }),
    );
    setProgress(null);
    if (res.error) setError(res.error);
  };

  const onDragEnd = (event: DragEndEvent) => {
    setDrag(null);
    const { active, over } = event;
    if (!over) return;
    const data = active.data.current as ConversationDragData | undefined;
    if (data?.type === 'conversation') {
      void handleConversationDrop(
        data,
        over.data.current as FolderDropData | UnifiedRoleDropData | undefined,
      );
      return;
    }
    // Account reorder (draggables without data.type): existing behavior.
    if (active.id === over.id) return;
    const ids = orderAccounts(accounts, accountOrder).map((a) => a.id);
    const from = String(active.id);
    const to = String(over.id);
    if (ids.includes(from) && ids.includes(to)) {
      setAccountOrder(moveId(ids, from, to));
    }
  };

  return (
    <DndContext
      sensors={sensors}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => setDrag(null)}
    >
      {children}
      <DragOverlay dropAnimation={null}>
        {drag ? (
          <div className="flex max-w-64 items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm shadow-md">
            <span className="truncate">{drag.subject || '—'}</span>
            {drag.count > 1 ? <Badge variant="secondary">{drag.count}</Badge> : null}
          </div>
        ) : null}
      </DragOverlay>
      {progress ? (
        <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2 rounded-full border bg-popover px-3 py-1.5 text-xs shadow-md">
          {t(locale, 'mail.movingMessages', { done: progress.done, total: progress.total })}
        </div>
      ) : null}
      {error ? (
        <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2 rounded-full border border-destructive/40 bg-destructive/10 px-3 py-1.5 text-xs text-destructive shadow-md">
          {error}
        </div>
      ) : null}
    </DndContext>
  );
}
