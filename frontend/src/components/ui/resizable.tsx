import { GripVertical } from 'lucide-react';
import type { ComponentProps } from 'react';
import * as ResizablePrimitive from 'react-resizable-panels';

import { cn } from '@/lib/utils';

function ResizablePanelGroup({
  className,
  ...props
}: ComponentProps<typeof ResizablePrimitive.Group>) {
  return (
    <ResizablePrimitive.Group
      data-slot="resizable-panel-group"
      className={cn('flex h-full w-full data-[panel-group-direction=vertical]:flex-col', className)}
      {...props}
    />
  );
}

function ResizablePanel({ ...props }: ComponentProps<typeof ResizablePrimitive.Panel>) {
  return <ResizablePrimitive.Panel data-slot="resizable-panel" {...props} />;
}

function ResizableHandle({
  withHandle,
  className,
  ...props
}: ComponentProps<typeof ResizablePrimitive.Separator> & {
  withHandle?: boolean;
}) {
  return (
    <ResizablePrimitive.Separator
      data-slot="resizable-handle"
      className={cn(
        // Visually a pure 1px hairline on the pane boundary; the 7px box is
        // the invisible drag target overlapping the left/top pane by 3px.
        'group relative z-10 -ml-[3px] flex w-[7px] shrink-0 cursor-col-resize items-center justify-center bg-transparent transition-colors',
        'after:absolute after:inset-y-0 after:left-[3px] after:w-px after:bg-border after:transition-colors',
        'hover:after:bg-[#c8c9cd] active:after:bg-[#c8c9cd]',
        'focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:outline-hidden',
        'data-[panel-group-direction=vertical]:-mt-[3px] data-[panel-group-direction=vertical]:-ml-0 data-[panel-group-direction=vertical]:h-[7px] data-[panel-group-direction=vertical]:w-full data-[panel-group-direction=vertical]:cursor-row-resize',
        'data-[panel-group-direction=vertical]:after:top-[3px] data-[panel-group-direction=vertical]:after:left-0 data-[panel-group-direction=vertical]:after:h-px data-[panel-group-direction=vertical]:after:w-full',
        'data-[panel-group-direction=vertical]:after:translate-x-0',
        '[&[data-panel-group-direction=vertical]>div]:rotate-90',
        className,
      )}
      {...props}
    >
      {withHandle ? (
        <div className="z-10 flex h-4 w-3 items-center justify-center rounded-sm border border-border bg-card opacity-0 transition-opacity group-hover:opacity-100 group-active:opacity-100">
          <GripVertical className="size-2.5 text-muted-foreground" />
        </div>
      ) : null}
    </ResizablePrimitive.Separator>
  );
}

export { ResizablePanelGroup, ResizablePanel, ResizableHandle };
