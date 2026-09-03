# Confirm Dialog Implementation Plan

> **For agentic workers:** Implement task-by-task. Steps use checkbox syntax.

**Goal:** Replace all `window.confirm` destructive prompts with a shared in-app Confirm dialog.

**Architecture:** Imperative `confirmAction()` Promise + root `ConfirmDialogHost` on Radix Dialog. Trash helper becomes async; Settings delete handlers await the same API.

**Tech Stack:** React, Radix Dialog (existing), vitest, en/zh i18n

## Global Constraints

- Preserve Lyra chrome: hairline, quiet gray; destructive = muted-red text/outline, not solid fill CTA.
- No `window.confirm` for trash / account delete / key delete.
- Host must mount before any confirm call (RootLayout).

---

### Task 1: `confirmAction` + host UI

**Files:**
- Create `frontend/src/lib/confirm-action.ts`
- Create `frontend/src/components/confirm-dialog-host.tsx`
- Modify `frontend/src/router.tsx` (mount host in `RootLayout`)
- Create `frontend/src/lib/confirm-action.test.ts`

- [ ] Write tests for resolve true/false and missing-host → false
- [ ] Implement registry + `confirmAction`
- [ ] Implement `ConfirmDialogHost` (Dialog, no X, Cancel + Confirm)
- [ ] Mount in RootLayout

### Task 2: Wire trash + Settings deletes

**Files:**
- Modify `frontend/src/lib/confirm-trash.ts` (+ test)
- Modify `mail-display.tsx`, `mail-list.tsx`, `conversation-context-menu.tsx`
- Modify `settings-page.tsx`, `encryption-settings.tsx`

- [ ] `confirmMoveToTrash` → `async` → `confirmAction({ tone: 'destructive' })`
- [ ] Await at all trash call sites
- [ ] Replace Settings/encryption `confirm(...)` with `confirmAction`
- [ ] Run `cd frontend && npm test -- confirm`
