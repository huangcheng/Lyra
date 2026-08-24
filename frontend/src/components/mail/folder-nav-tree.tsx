/**
 * Expand/collapse folder tree for single-account custom folders.
 */

import { ChevronRight, File } from 'lucide-react';
import { useState } from 'react';

import { buttonVariants } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import type { FolderTreeNode } from '@/lib/folder-tree';
import { cn } from '@/lib/utils';

interface FolderNavTreeProps {
  isCollapsed: boolean;
  nodes: FolderTreeNode[];
  selectedFolderId: string | null;
  onSelect: (folderId: string) => void;
}

function FolderTreeRow({
  node,
  depth,
  isCollapsed,
  selectedFolderId,
  onSelect,
  expanded,
  onToggle,
}: {
  node: FolderTreeNode;
  depth: number;
  isCollapsed: boolean;
  selectedFolderId: string | null;
  onSelect: (folderId: string) => void;
  expanded: boolean;
  onToggle: () => void;
}) {
  const hasChildren = node.children.length > 0;
  const isActive = selectedFolderId === node.id;
  const variant = isActive ? 'default' : 'ghost';
  const className = cn(
    buttonVariants({ variant, size: isCollapsed ? 'icon' : 'sm' }),
    isCollapsed ? 'h-9 w-9' : 'h-9 w-full justify-start gap-1 px-2 has-[>svg]:px-2',
    variant === 'default' &&
      'dark:bg-muted dark:text-white dark:hover:bg-muted dark:hover:text-white',
  );

  const content = isCollapsed ? (
    <>
      <File className="h-4 w-4" />
      <span className="sr-only">{node.title}</span>
    </>
  ) : (
    <>
      {hasChildren ? (
        <button
          type="button"
          className="flex h-5 w-5 shrink-0 items-center justify-center rounded-sm hover:bg-accent"
          style={{ marginLeft: depth * 12 }}
          onClick={(event) => {
            event.stopPropagation();
            onToggle();
          }}
          aria-expanded={expanded}
          aria-label={expanded ? 'Collapse folder' : 'Expand folder'}
        >
          <ChevronRight
            className={cn('h-3.5 w-3.5 transition-transform', expanded && 'rotate-90')}
          />
        </button>
      ) : (
        <span className="inline-block w-5 shrink-0" style={{ marginLeft: depth * 12 }} />
      )}
      <File className="h-4 w-4 shrink-0" />
      <span className="truncate">{node.title}</span>
      {node.label ? (
        <span
          className={cn(
            'ml-auto font-normal tabular-nums',
            variant === 'default' && 'text-background dark:text-white',
          )}
        >
          {node.label}
        </span>
      ) : null}
    </>
  );

  const row = (
    <button type="button" className={className} onClick={() => onSelect(node.id)}>
      {content}
    </button>
  );

  if (isCollapsed) {
    return (
      <Tooltip delayDuration={0}>
        <TooltipTrigger asChild>{row}</TooltipTrigger>
        <TooltipContent side="right" className="flex items-center gap-4">
          {node.title}
          {node.label ? <span className="ml-auto text-muted-foreground">{node.label}</span> : null}
        </TooltipContent>
      </Tooltip>
    );
  }

  return row;
}

function FolderTreeBranch({
  node,
  depth,
  isCollapsed,
  selectedFolderId,
  onSelect,
  expandedIds,
  toggleExpanded,
}: {
  node: FolderTreeNode;
  depth: number;
  isCollapsed: boolean;
  selectedFolderId: string | null;
  onSelect: (folderId: string) => void;
  expandedIds: Set<string>;
  toggleExpanded: (id: string) => void;
}) {
  const expanded = expandedIds.has(node.id);

  return (
    <div className="grid gap-1">
      <FolderTreeRow
        node={node}
        depth={depth}
        isCollapsed={isCollapsed}
        selectedFolderId={selectedFolderId}
        onSelect={onSelect}
        expanded={expanded}
        onToggle={() => toggleExpanded(node.id)}
      />
      {!isCollapsed && expanded && node.children.length > 0
        ? node.children.map((child) => (
            <FolderTreeBranch
              key={child.id}
              node={child}
              depth={depth + 1}
              isCollapsed={isCollapsed}
              selectedFolderId={selectedFolderId}
              onSelect={onSelect}
              expandedIds={expandedIds}
              toggleExpanded={toggleExpanded}
            />
          ))
        : null}
    </div>
  );
}

export function FolderNavTree({
  isCollapsed,
  nodes,
  selectedFolderId,
  onSelect,
}: FolderNavTreeProps) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(() => new Set());

  const toggleExpanded = (id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <div
      data-collapsed={isCollapsed}
      className="group flex flex-col gap-4 py-2 data-[collapsed=true]:py-2"
    >
      <nav className="grid gap-1 px-2 group-data-[collapsed=true]:justify-center group-data-[collapsed=true]:px-2">
        {nodes.map((node) => (
          <FolderTreeBranch
            key={node.id}
            node={node}
            depth={0}
            isCollapsed={isCollapsed}
            selectedFolderId={selectedFolderId}
            onSelect={onSelect}
            expandedIds={expandedIds}
            toggleExpanded={toggleExpanded}
          />
        ))}
      </nav>
    </div>
  );
}
