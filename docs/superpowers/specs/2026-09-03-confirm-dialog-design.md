# Confirm dialog (replace native `window.confirm`)

Date: 2026-09-03

## Problem

Destructive actions (trash mail, delete account, delete OpenGPG key) use `window.confirm()`, which breaks brand chrome and looks like a browser system dialog.

## Decision

Ship a shared in-app confirm dialog built on the existing Radix `Dialog`, plus an imperative `confirmAction()` Promise API so call sites stay close to today’s `if (!confirm) return` shape (async).

## Scope

- Mail trash (single + bulk) via `confirmMoveToTrash`
- Settings → delete account
- Settings → encryption → delete key

Out of scope: non-destructive dialogs (compose, unlock, edit account).

## UX

- Centered modal, quiet hairline border, cool gray surface (match existing Dialog).
- Title = existing confirm copy; optional description unused for v1 copy.
- Footer: Cancel (outline) + Confirm/Delete.
- Destructive tone: outline button with muted-red text/border (not a solid filled CTA).
- Escape, overlay click, and Cancel resolve `false`; Confirm resolves `true`.
- Focus trap + `aria` from Radix Dialog; no close (X) button on confirm (avoids ambiguity).

## API

```ts
confirmAction({
  title: string;
  description?: string;
  confirmLabel?: string; // default: common.confirm
  cancelLabel?: string;  // default: common.cancel
  tone?: 'default' | 'destructive';
}): Promise<boolean>
```

Host component mounted once in the app root. If the host is missing, resolve `false` (safe no-op).

## Non-goals

- Prompt / input dialogs
- Nested confirms
- Remember “don’t ask again”
