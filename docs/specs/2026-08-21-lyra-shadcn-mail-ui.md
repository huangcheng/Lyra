# Lyra — shadcn mail UI + unified inbox

**Date:** 2026-08-21  
**Status:** Accepted  
**Supersedes:** custom chrome in `docs/specs/2026-08-21-lyra-ui-design.md` (palette / editorial login). Product UI is the [shadcn mail example](https://v3.shadcn.com/examples/mail) and [login-01](https://ui.shadcn.com/blocks/login).

## Decision

Do not invent a parallel design system. Ship:

1. **Mail** — v3 shadcn mail three-pane layout (account switcher, folder nav, All/Unread list, reading pane with reply box).
2. **Login** — shadcn `login-01` card (username/password). Same card for bootstrap and TOTP. No Google / forgot-password / sign-up (single-user self-host).
3. **Unified inbox** — the example already switches accounts; Lyra adds **All inboxes** as the default view.

The mail chrome follows the v3 example source (`apps/www/app/(app)/examples/mail` on the `v3` branch): account switcher (icon + name), folder nav with unread badges, a second nav group, All/Unread tabs, search, card list (sender, preview, relative time, unread dot, badges), and a reading pane with archive/junk/trash/snooze, reply/reply-all/forward, More menu, avatar, Reply-To, reply box, mute switch, Send. Lyra-only: All inboxes, Contacts/Calendar/Settings instead of demo Gmail categories, compose icon next to the folder title, real `/api/v1` data. Logout and locale live in Settings. Do not copy the docs-site top nav (Examples / Dashboard / …).

## Unified inbox

| Item | Behavior |
|------|----------|
| Default view | `All inboxes` (account id `all`) |
| Folders | Standard roles merged across accounts: Inbox, Drafts, Sent, Spam, Trash, Archive. Unread counts are sums. |
| List | `GET /api/messages?role=inbox` (optional `accountId`). Rows show an account badge when unified. |
| Single account | Switcher still lists each mail account; folders and list are that account only. |
| Compose | If unified, pick From account (first account default). |
| Sync | Unified syncs every account. |

Custom IMAP folders appear only when a single account is selected.

## Stack (frontend)

Vite + React + Tailwind v4 + shadcn (Radix) + lucide-react. Path alias `@/`. Geist font.

## Out of scope

- Recreating Linear / postage-stamp / COBE auth chrome
- Native apps
- Shared inboxes (collaboration)
