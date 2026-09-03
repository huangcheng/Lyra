# Sync progress details — design

Date: 2026-09-02
Status: approved (brainstorming)

## Goal

Show users what a sync is doing while it runs: which account is syncing, which
folder, how many messages fetched, and what failed — plus a visible "syncing"
marker on the account row in the sidebar.

## Non-goals

- No new backend events, endpoints, or migrations. The SSE stream already emits
  everything needed (`sync_started`, `folder_progress`, `folder_complete`,
  `incremental_complete`, `sync_error`, `sync_complete`).
- No historical sync log. The popover reflects the current/last activity held
  in memory; refresh resets it. `lastSyncAt` (already on the account record)
  covers "when did this last succeed".
- No per-folder progress list inside the popover — only the currently syncing
  folder per account.

## Existing pieces

- SSE stream: `frontend/src/rxjs/sync-events.ts` (`syncEvents$`), event union in
  `frontend/src/types/index.ts:123`.
- `frontend/src/lib/use-syncing-accounts.ts` — set of currently syncing account
  ids, with a pure `reduceSyncEvent` (the pattern to extend).
- `SyncAllButton` (`frontend/src/components/mail/sync-all-button.tsx`) — sidebar
  footer button, already spins while any account syncs.
- Account header rows: `frontend/src/components/mail/sidebar-folders.tsx`
  (`AccountSection`, ~line 352).
- Folder names resolve through `useMailStore((s) => s.folders)`
  (`Record<string, MailFolder>`); accounts carry `lastSyncAt`.

## Architecture

### 1. `src/lib/sync-progress.ts` (new, pure)

- `AccountSyncStatus` type:
  - `state: 'syncing' | 'error'` (idle = account absent from the map)
  - `currentFolderId: string | null`
  - `fetched: number`, `total: number` (from latest `folder_progress`)
  - `error: string | null`
- `reduceSyncProgress(map, event): map` — pure reducer over `SyncEvent`:
  - `sync_started` → `{ state: 'syncing', currentFolderId: null, fetched: 0, total: 0, error: null }`
  - `folder_progress` → update currentFolderId/fetched/total
  - `folder_complete` / `incremental_complete` → clear currentFolderId (keep counts)
  - `sync_error` → `{ state: 'error', error }`, clear currentFolderId
  - `sync_complete` → back to `idle`, zeroed counters
- `useSyncProgress(): ReadonlyMap<string, AccountSyncStatus>` — subscribes to
  `syncEvents$`, mirrors `use-syncing-accounts.ts`. (That hook stays as-is;
  `SyncAllButton` keeps using it for the global spin.)
- Colocated vitest tests per reducer transition, incl. unknown account ids and
  events arriving without a preceding `sync_started`.

### 2. Sync status popover (sidebar footer)

- `SyncAllButton` gains a popover (shadcn `Popover`, already in `components/ui`).
  The existing RefreshCw click still triggers sync-all; a small chevron/status
  affordance next to it opens the popover.
- Popover content, one section per account:
  - display name (fallback email address)
  - syncing: spinner + `Syncing <folder name> — <fetched>/<total>`; folder name
    resolved from the mail store, falls back to a generic "folder" label when
    the id is unknown; while no folder event has arrived, show "Starting…"
  - idle: `Last synced <relative time>` from `account.lastSyncAt`, or
    `Not synced yet` when absent
  - error: `error` string in destructive color
  - a `Sync now` row action per account (`POST /accounts/:id/sync`, same
    endpoint the sync-all button loops over)
- en + zh strings in `src/i18n`.

### 3. Sidebar account spinner

- In `AccountSection` (`sidebar-folders.tsx`), when `useSyncProgress()` reports
  the account `syncing`, render a `Loader2 animate-spin` icon in front of the
  account name (before the truncated name span, after the chevron).

## Error handling

- `sync_error` moves the account to `error` state with the server-provided
  message; the next `sync_started` for that account clears it.
- Unknown `folderId` in progress events degrades to a generic label, never a
  crash.
- Events for unknown accounts are still tracked (defensive: account list may
  lag the stream).

## Testing

- Vitest for `reduceSyncProgress` covering every event type and ordering edge
  cases (progress without start, error then restart, interleaved accounts).
- No backend changes; `npm run check` (oxlint + tsc + vitest) is the gate.

## Notes

- zh translations required alongside en.
- Popover and spinner consume the same `useSyncProgress` hook — one event
  source (shared hot subject); each consumer derives its own map.
