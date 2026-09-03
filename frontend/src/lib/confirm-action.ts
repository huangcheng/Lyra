/**
 * Imperative confirm dialog — Promise API for destructive / irreversible actions.
 * Requires {@link ConfirmDialogHost} mounted in the app root.
 */

export type ConfirmTone = 'default' | 'destructive';

export type ConfirmActionOptions = {
  title: string;
  description?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: ConfirmTone;
};

export type ConfirmPending = ConfirmActionOptions & {
  resolve: (value: boolean) => void;
};

type PendingSetter = (pending: ConfirmPending | null) => void;

let pendingSetter: PendingSetter | null = null;

/** Called by {@link ConfirmDialogHost}; pass `null` on unmount. */
export function registerConfirmHost(setter: PendingSetter | null): void {
  pendingSetter = setter;
}

/**
 * Open the shared confirm dialog. Resolves `true` on confirm, `false` on
 * cancel / Escape / overlay dismiss. If no host is mounted, resolves `false`.
 */
export function confirmAction(options: ConfirmActionOptions): Promise<boolean> {
  if (!pendingSetter) {
    return Promise.resolve(false);
  }
  return new Promise<boolean>((resolve) => {
    pendingSetter!({ ...options, resolve });
  });
}
