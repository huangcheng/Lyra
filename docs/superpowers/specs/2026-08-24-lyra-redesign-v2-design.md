# Lyra UI Redesign v2 — Design

Date: 2026-08-24
Status: approved (design, reviewed in Ardot)
Scope: frontend (`frontend/`), plus one additive read-only backend endpoint (`GET /api/v1/messages/stats`) that aggregates already-synced local mail metadata for the dashboard. No changes to sync, auth, or existing endpoint behavior.

Supersedes: `docs/specs/2026-08-21-lyra-ui-design.md`, `docs/specs/2026-08-21-lyra-ardot-review.md`, `docs/superpowers/plans/2026-08-21-lyra-ui-shell.md`. The shadcn-stock mail look and the indigo accent from `2026-08-24-lyra-ui-polish-design.md` are replaced by this design.

Source of truth for visuals: Ardot file 716978471157674, page **"Lyra · Redesign v2"** (`7:1`) — 10 frames (light + dark for each): Mail, Login, Settings ×3 (Accounts, Spam & Filters, Privacy), Dashboard.

## Background

The v1 UI (stock shadcn mail + indigo accent) was reviewed in the browser and judged too rough/generic. This redesign was iterated directly in Ardot with user review: mail, login, settings (accounts / spam / privacy), and an email-analytics dashboard, each in light and dark.

Decisions made during review:

- Folder-tree sidebar (macOS Mail / Thunderbird style) replaces the flat nav — user's explicit choice over the flat design.
- Settings and Dashboard are **standalone pages**, not modes inside mail — no mail sidebar on either.
- Dashboard and Settings stay **separate destinations** sharing one slim-nav pattern (not merged, no global icon rail).
- The logo is the **stamp**: a solid ink square with a serif "L". It appears on the login card, the mail sidebar footer, and the top of every standalone-page nav. It inverts between light and dark.
- No solid black CTAs: primary actions are white pills with a hairline border.

## Design language

### Color tokens

| Token | Light | Dark | Use |
|---|---|---|---|
| INK | `#1A1B1F` | `#ECEDEF` | Primary text, stamp square, chart bars |
| SEC | `#6B6F76` | `#9BA0A8` | Secondary text, counts |
| TER | `#9B9BA3` | `#6E737B` | Tertiary text, inactive icons, placeholders |
| HAIR | `#E2E2E5` | `#2E3138` | 1px hairline rules, card borders |
| BTNB | `#E1E2E4` | `#35383F` | Button/input borders |
| HOV | `#E8E8E9` | `#26282E` | Hover, active nav pill, segmented track |
| PANEL | `#EFF0F2` | `#1A1C20` | Sidebar background, avatar tiles |
| LIST | `#F6F6F8` | `#17181C` | List column / standalone nav background |
| READER | `#FFFFFF` | `#24262B` | Reading surface, cards, pills |
| Canvas | `#F7F7F8` | `#101114` | Login page backdrop |
| UNREAD | `#E2A336` | `#E2A336` | Unread dot, "today" bar (same both themes) |
| OK | `#3D9A5F` | `#3D9A5F` | Sync dot, toggle-on (same both themes) |
| Danger | `#B4453C` | `#D4756B` | Destructive text only — the only red anywhere |

Color is reserved for status: amber = unread/today, green = healthy/on, muted red = destructive. Everything else is cool gray.

### Typography

- **Inter** (Regular / Medium / SemiBold) — UI text.
- **Inter Tight Medium** — page titles, KPI numerals.
- **Instrument Serif** — the "Lyra" wordmark and the stamp "L" only.
- Chinese renders natively via system fallbacks; all UI strings in `en.json` + `zh.json`.

### Primitives

- **Stamp logo**: ink square (cornerRadius ≈ 20–25% of size) with serif "L" in the surface color. In dark: light square, dark L.
- **Buttons**: white/raised pills, 1px BTNB border, radius 7–8, ink text. Never solid black, never brand-colored.
- **Inputs**: surface fill, BTNB border, radius 8, TER placeholder.
- **Cards**: READER fill, HAIR border, radius 10.
- **Toggles**: 36×20 pill; on = OK green, off = neutral gray; white knob.
- **Segmented control**: HOV track, READER active pill.
- **Hairlines**: 1px HAIR rects divide regions; no shadows anywhere.
- **Icons**: thin-stroke lucide-style line icons, TER when inactive / INK when active.

## Screens

### Login

Centered 380px white card on the canvas color: stamp (40px) + serif wordmark, tagline "Mail you host yourself.", username/password inputs, Log In white pill with ink border, footer with EN / 中文 toggle and "self-hosted · v{version}".

### Mail (three-pane)

- **Sidebar (240px, PANEL)**: account pill + compose button on top. **UNIFIED** section: Inbox · Drafts · Sent · Trash with right-aligned unread counts. **ACCOUNTS** section: one collapsible header per account (chevron, name — provider, total unread), containing special folders with their icons (Inbox tray, Archive box, Spam flag, Trash bin) and user folders with a plain folder glyph, nested with 16px indent per level and disclosure chevrons. Footer: stamp · Lyra · sync dot — then icon buttons for Dashboard (chart), Settings (gear), theme (moon/sun).
- **List column (400px, LIST)**: "Inbox" title, All mail / Unread segmented tabs, search input; message rows with avatar initials, amber unread dot, subject/preview, label chips; selected row = white card with BTNB border.
- **Reader (READER)**: toolbar of hairline icon buttons (archive, spam, trash, snooze, reply, reply-all, forward; star + overflow right); message header with avatar, sender, timestamp; remote-image privacy banner ("Remote images are hidden… Show images · Always allow this sender"); body; bottom reply box with Send pill.
- No folder sizes in the sidebar (that's settings-level info); sync status is the green dot (detail moves to Settings/tooltip).

### Dashboard (standalone)

Slim nav (220px, LIST): stamp + Lyra, "← Mail" return, DASHBOARD section with Overview. Content: header with title + "Your email at a glance" + range segmented (7/30/90 days); KPI cards (Unread, Received today, Sent this week, Storage with ink meter); Volume bar chart — 14 days, ink bars, today in amber, day letters below; Top senders list; By-account share bars. Data comes from a new additive read-only endpoint (`GET /api/v1/messages/stats?days=`) aggregating the local DB; cards show designed empty states when data is missing. Storage has no size endpoint yet — that KPI renders its empty state.

### Settings (standalone)

Same slim nav: stamp + Lyra, "← Mail", sections General · Accounts · Spam & Filters · Privacy · Appearance.

- **Accounts**: card per connected account (avatar tile, address, green sync dot + last-synced + protocol badge JMAP/IMAP, Manage button); Add account card; default sending account selector.
- **Spam & Filters**: toggles (enable filtering, learn from actions, auto-delete after 30 days), sensitivity segmented (Lenient/Standard/Strict), blocked-senders list with inline add/remove. The backend has no spam-filtering settings yet — these controls render **disabled with a "Soon" badge** until the plugin kernel lands; only the layout and copy are real.
- **Privacy**: Remote content (block remote images by default, proxy through Lyra — backed by the existing `GET/PATCH /settings/privacy`), Tracking protection (strip pixels, warn on rewritten links — disabled with "Soon"), Your data (Export / Delete — disabled with "Soon").

## Navigation model

Mail is the home. Dashboard and Settings are reached via the sidebar-footer icon buttons, and return via "← Mail" in their slim nav. Login stands alone. All standalone pages share the identical slim-nav shell (brand, back, section list).

## Dark mode

Every screen exists in both themes per the token table above; the stamp, charts, and buttons invert by token, status colors stay constant. Theme switching stays as implemented (`dark` class on `<html>`, localStorage, light/dark/system) — the sidebar-footer moon/sun button cycles it.

## Verification

- `cd frontend && npm run check` (tsc + oxlint + prettier) clean.
- Browser walkthrough (agent-browser) against Docker at `http://127.0.0.1:3000`: login → mail (folder tree expand/collapse, select, reply box) → dashboard → settings sections; light and dark; en and zh.
- Screenshots compared against the Ardot frames on page `7:1`.
