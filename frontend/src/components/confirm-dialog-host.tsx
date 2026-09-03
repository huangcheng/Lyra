/**
 * Root-mounted host for {@link confirmAction}. Renders one shared Dialog.
 */

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { registerConfirmHost, type ConfirmPending } from '@/lib/confirm-action';
import { t } from '@/i18n';
import { cn } from '@/lib/utils';
import { useUIStore } from '@/stores/ui';

export function ConfirmDialogHost() {
  const locale = useUIStore((s) => s.locale);
  const [pending, setPending] = useState<ConfirmPending | null>(null);

  useEffect(() => {
    registerConfirmHost(setPending);
    return () => registerConfirmHost(null);
  }, []);

  function settle(value: boolean) {
    pending?.resolve(value);
    setPending(null);
  }

  const open = pending !== null;
  const tone = pending?.tone ?? 'default';
  const confirmLabel = pending?.confirmLabel ?? t(locale, 'common.confirm');
  const cancelLabel = pending?.cancelLabel ?? t(locale, 'common.cancel');

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) settle(false);
      }}
    >
      <DialogContent showCloseButton={false} className="sm:max-w-md" aria-describedby={undefined}>
        <DialogHeader>
          <DialogTitle className="text-base font-semibold tracking-tight">
            {pending?.title}
          </DialogTitle>
          {pending?.description ? (
            <DialogDescription>{pending.description}</DialogDescription>
          ) : null}
        </DialogHeader>
        <DialogFooter className="gap-2">
          <Button type="button" variant="outline" size="sm" onClick={() => settle(false)}>
            {cancelLabel}
          </Button>
          <Button
            type="button"
            size="sm"
            variant={tone === 'destructive' ? 'outline' : 'default'}
            className={cn(
              tone === 'destructive' &&
                'border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive',
            )}
            onClick={() => settle(true)}
            autoFocus
          >
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
