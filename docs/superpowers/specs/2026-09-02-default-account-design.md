# Default mail account — design

Date: 2026-09-02
Status: approved (pending spec review)

## Problem

Two related gaps around which account a compose is sent from:

1. **No user-chosen default account.** When composing a new message from the
   unified "All inboxes" view, the From account is `accounts[0]` — whatever
   the server happens to return first (`compose-dialog.tsx`,
   `effectiveFrom = selectedAccountId === ALL_ACCOUNTS ? accounts[0]?.id : …`).
   Thunderbird solves this with a "Set as Default" account setting; we want
   the same.
2. **Reply/forward From the wrong account in unified view.** The compose seed
   ignores which account received the message: reply/forward builders in
   `lib/compose-draft.ts` know the source message's account (they already use
   it for the signature), but `ComposeDraft` carries no `accountId`, so the
   From selector still falls back to `accounts[0]`.

## Decisions

- **Placement: Settings → Accounts only.** Each account row gets a
  "Set as default" action; the current default shows a star badge. The mail
  sidebar stays clean (no star toggle there).
- **Persistence: `lyra_user.ui_state` JSON blob** via the existing
  `PATCH /api/v1/auth/preferences` seam — the same place sidebar account
  order (`accountOrder`) already lives. No backend migration.
  - Rejected alternative: an `is_default` column on `mail_account`. Default
    account is a *per-user preference*; a column on the account row would be
    shared across users once multi-user lands, while `ui_state` already lives
    on `lyra_user`. It is also purely client-facing state, which is what
    `ui_state` is for.
- **Replies use the receiving account.** `ComposeDraft` gains an optional
  `accountId`; the default account is only the fallback for brand-new compose
  with no source context.

## Data model & flow

### UI store

`useUIStore` gains:

```ts
defaultAccountId: string | null
setDefaultAccount(id: string | null): void
```

### Persistence (`frontend/src/lib/persist-view-state.ts`)

- `applyViewState` restores `defaultAccountId` when it is a string;
  invalid non-strings are ignored and an absent key leaves the current
  value untouched. No cross-check against the account list at
  restore time; stale ids (deleted accounts) simply fail the lookup at use
  time and fall back — same pattern as `accountOrder`.
- The debounced PATCH body includes `defaultAccountId`, and the store
  subscription watches it for change detection.

### From-account resolution

New pure helper in `frontend/src/lib/` (e.g. `resolve-from-account.ts`),
unit-testable:

```ts
resolveFromAccountId(opts: {
  draftAccountId?: string;      // reply/forward/draft source account
  selectedAccountId: string;    // sidebar selection or ALL_ACCOUNTS
  defaultAccountId: string | null;
  accounts: MailAccount[];
}): string
```

Resolution order (first hit wins; every candidate must exist in `accounts`):

1. `draftAccountId` — reply/forward/edit-draft always send from the
   receiving/owning account.
2. `selectedAccountId` when it is not `ALL_ACCOUNTS` — the account the user
   is browsing.
3. `defaultAccountId` when set and still present.
4. `accounts[0]?.id ?? ''` — today's behavior as the final fallback.

`compose-dialog.tsx` seeding replaces the inline `effectiveFrom` expression
with this helper. The signature lookup already follows `effectiveFrom`, so it
stays consistent automatically.

### ComposeDraft change

`frontend/src/lib/compose-draft.ts`: `ComposeDraft` gains
`accountId?: string`; the reply, reply-all, forward, and edit-draft builders
set it from the source message's `accountId`. New-compose drafts leave it
undefined.

## Settings UI

In `settings-page.tsx` Accounts section:

- Default account row: small filled star icon + `Default` / `默认` label.
- Other rows: ghost button `Set as default` / `设为默认` next to the existing
  actions; clicking calls `setDefaultAccount(id)` (persistence is automatic
  via the debounced subscription).
- Both strings added to `frontend/src/i18n/en.json` and `zh.json`.

## Error handling

- Persistence is fire-and-forget like the rest of `ui_state`; a failed save
  just means the previous default restores on next load.
- If the default account is deleted, the stale id falls back to
  `accounts[0]` silently. No cleanup write is needed; the next explicit
  "Set as default" overwrites it.

## Testing

- Unit tests (vitest, colocated in `frontend/src/lib/`):
  - `resolveFromAccountId`: each resolution-order branch, missing draft
    account, deleted default account, empty account list.
  - `applyViewState`: valid/invalid `defaultAccountId` restore.
- Verification gate: `cd frontend && npm run check` (tsc -b + oxlint +
  prettier) and `npx vitest run`. **Do not rely on `npx tsc --noEmit`** —
  this repo's project-reference tsconfig under-checks it.
- Live check (Vite dev server, proxying to the Docker backend on :3000):
  set Outlook as default → compose from All inboxes shows Outlook in From;
  reply to a QQ-received message from unified view shows QQ in From.

## Out of scope

- Per-folder From overrides.
- Thunderbird-style full identity management (multiple identities per
  account, per-identity name/signature). The existing per-account signature
  stays as-is.
- Sidebar star toggle or account-list reordering changes.
