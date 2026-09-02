/**
 * Drag-and-drop context for the mail shell.
 *
 * One DndContext covers both panes with drag-kind-aware collision:
 * - conversation rows (useDraggable in mail-list) drop onto folder rows
 *   (useDroppable in sidebar-folders) via pointer-within targeting;
 * - account sections sort by their header rows only, so an expanded
 *   section's folder droppables can never eclipse sibling headers.
 * Drag kinds are told apart by `active.data.current?.type`.
 */

import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  TouchSensor,
  closestCenter,
  pointerWithin,
  rectIntersection,
  useSensor,
  useSensors,
  type Announcements,
  type CollisionDetection,
  type DragEndEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { sortableKeyboardCoordinates } from '@dnd-kit/sortable';
import { useEffect, useRef, useState, type ReactNode } from 'react';

import { Badge } from '@/components/ui/badge';
import { t } from '@/i18n';
import { moveId, orderAccounts } from '@/lib/account-order';
import {
  moveMessages,
  resolveRoleFolder,
  type ConversationDragData,
} from '@/lib/conversation-actions';
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
  // Pointer for mouse/pen; TouchSensor with a long-press delay so list
  // scrolling still wins over drag on touch devices; KeyboardSensor makes
  // the screen-reader reorder instructions true instead of decorative.
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 200, tolerance: 6 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const [drag, setDrag] = useState<ConversationDragData | null>(null);
  const [accountDrag, setAccountDrag] = useState<{ id: string; name: string } | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!error) return;
    const handle = window.setTimeout(() => setError(null), 6000);
    return () => window.clearTimeout(handle);
  }, [error]);

  /**
   * Conversations target whatever folder row the pointer is inside (falling
   * back to rect intersection for touch, where the finger rect is the row).
   * Account sorts consider only sibling account headers — folder droppables
   * inside an expanded section must not steal the collision.
   */
  const collisionDetection: CollisionDetection = (args) => {
    if (args.active.data.current?.type === 'conversation') {
      const within = pointerWithin(args);
      return within.length > 0 ? within : rectIntersection(args);
    }
    const ids = new Set(orderAccounts(accounts, accountOrder).map((a) => a.id));
    const sortableOnly = {
      ...args,
      droppableContainers: args.droppableContainers.filter((c) => ids.has(String(c.id))),
    };
    return closestCenter(sortableOnly);
  };

  const accountName = (id: string): string => {
    const a = accounts.find((x) => x.id === id);
    return (a?.displayName || a?.emailAddress || id) as string;
  };

  const orderedIds = () => orderAccounts(accounts, accountOrder).map((a) => a.id);

  // Mirrors whether a drag is live so the cancel announcement stays quiet
  // for conversations (which announce nothing).
  const activeDragRef = useRef(false);

  const announcements: Announcements = {
    onDragStart: ({ active }) => {
      if (active.data.current?.type === 'conversation') return;
      return t(locale, 'mail.dndGrabbed', { name: accountName(String(active.id)) });
    },
    onDragEnd: ({ active, over }) => {
      if (active.data.current?.type === 'conversation' || !over) return;
      const ids = orderedIds();
      const position = ids.indexOf(String(over.id)) + 1;
      return t(locale, 'mail.dndDropped', {
        name: accountName(String(active.id)),
        position: position > 0 ? position : ids.length,
        total: ids.length,
      });
    },
    onDragOver: () => undefined,
    onDragCancel: () => (activeDragRef.current ? t(locale, 'mail.dndCanceled') : undefined),
  };

  const onDragStart = (event: DragStartEvent) => {
    const data = event.active.data.current as ConversationDragData | undefined;
    if (data?.type === 'conversation') {
      setDrag(data);
      activeDragRef.current = true;
      return;
    }
    const id = String(event.active.id);
    if (accounts.some((a) => a.id === id)) {
      setAccountDrag({ id, name: accountName(id) });
      activeDragRef.current = true;
    }
  };

  const dropFolderId = (
    data: ConversationDragData,
    overData: FolderDropData | UnifiedRoleDropData | undefined,
  ): string | null => {
    if (!overData || overData.type !== 'folder') return null;
    if ('unified' in overData && overData.unified) {
      // Unified role row: target is the drag account's folder with that role.
      const target = resolveRoleFolder(
        useMailStore.getState().folders,
        data.accountId,
        overData.role,
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
    setAccountDrag(null);
    activeDragRef.current = false;
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
      collisionDetection={collisionDetection}
      accessibility={{
        announcements,
        screenReaderInstructions: { draggable: t(locale, 'mail.dndInstructions') },
      }}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => {
        setDrag(null);
        setAccountDrag(null);
        activeDragRef.current = false;
      }}
    >
      {children}
      <DragOverlay dropAnimation={null}>
        {drag ? (
          <div className="flex max-w-64 items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm shadow-md">
            <span className="truncate">{drag.subject || '—'}</span>
            {drag.count > 1 ? <Badge variant="secondary">{drag.count}</Badge> : null}
          </div>
        ) : accountDrag ? (
          <div className="flex max-w-64 items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm shadow-md">
            <span className="text-ter-foreground">⋮⋮</span>
            <span className="truncate font-medium">{accountDrag.name}</span>
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
