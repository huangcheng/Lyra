# Lyra — UI design language

**Date:** 2026-08-21  
**Status:** Superseded by `docs/specs/2026-08-21-lyra-shadcn-mail-ui.md` (shadcn mail + login-01). Kept as historical reference.  
**Audience:** Design + frontend  
**Reference:** Linear-style cool neutrals (user-provided screenshot)

**Feel:** Quiet tool chrome. Cool light grays, near-black type, color only for status — never for brand paint.

**Keep:** postage stamp + serif **L** — mark uses a **sawtooth (perforated) edge**, not a plain rounded square.

---

## Palette

| Role | Hex | Notes |
|------|-----|--------|
| Canvas | `#F7F7F8` | page / structural field |
| Panel | `#F0F0F2` | sidebar, list column, chrome |
| Reader | `#F9F9FA` | reading / focus surface (lightly off chrome) |
| Surface / card | `#FFFFFF` | buttons, cards, elevated controls |
| Hover / select (in panel) | `#E8E8E9` | sidebar row hover/active |
| Card select (on panel) | `#FFFFFF` | selected list row as white card on gray panel |
| Rule | `#E2E2E5` | hairlines |
| Ink | `#1A1B1F` | primary text (soft charcoal, not `#000`) |
| Secondary | `#6B6F76` | meta, muted labels |
| Tertiary | `#9B9BA3` | placeholders, icons |
| Button fill | `#FFFFFF` | default control (Linear white) |
| Button hover / active | `#E8E8E9` | hover and selected / pressed |
| Button border | `#E1E2E4` | hairline on default white pills |

**Separation (minimal):**  
- Tone first (chrome vs `#F9F9FA` reader).  
- **1px hairlines** on white controls, selected list cards (`#E1E2E4` / `#E2E2E5`).  
- **Resizable splitters** between sidebar | list | reader (not a single decorative rule).  
- **No** bordered section headers.  
- **Shadows:** none by default; **whisper only** on the allowlist in `docs/superpowers/specs/2026-08-21-lyra-premium-kit-design.md` (selected list card, auth card, menus/compose).

**Controls (Linear pattern):** Buttons start **white** + thin `#E1E2E4` border, charcoal label. **Hover / active** → soft gray `#E8E8E9` (border optional / same family). Never solid black fill + white label.

**Status only** (mail unread / priority / sync — not chrome): amber `#E2A336`, green `#3D9A5F`, red `#D94C4C`. Do not use these on buttons, rails, or auth.

**Dark:** invert within the same cool gray family. Appearance = Light / Dark / System. No multi-theme packs.

---

## Auth

- Full canvas `#F7F7F8`, centered surface card, hairline rule.
- Stamp + wordmark (serif L / Lyra), muted tagline.
- Inputs: white + rule; CTA: white pill + border (hover/active = gray).
- Setup / Login / TOTP share the shell (no dark brand column).

## Mail

- Sidebar + list = gray chrome `#F0F0F2`.
- **Reading** = lightly different `#F9F9FA` (Linear-style focus surface), soft radius + hairline.
- Selected list row = white card on the gray list.
- Sidebar rows: default transparent; **hover / active = `#E8E8E9`**.
- Compose / Reply / Archive = white pills + border. No accent hue.

### Column splitters (resizable)

Users resize any pane — long subjects, wide reading, narrow nav.

| | Default | Min | Max |
|--|---------|-----|-----|
| Sidebar | ~232 | 180 | 320 |
| List | ~340 | 280 | 480 |
| Reader | fill | 360 | — |

- **Hit target:** 5px wide strip (chrome fill `#F0F0F2`).  
- **Idle:** 1px center rule `#E2E2E5`.  
- **Hover / drag:** rule → `#C8C9CD` (or 2px); cursor `col-resize`.  
- Persist widths in local prefs. Double-click splitter → reset that pane to default.  
- Two splitters: **sidebar|list** and **list|reader**. Same treatment both sides (sidebar/list used to blend with no rule).

### Rich mail row (Apple Mail–inspired, Lyra chrome)

Each list row (not a heavy card stack):

| Zone | Content |
|------|---------|
| Status | Fixed **12px** column left of avatar — reply / forward glyph when thread was answered; empty when not (keeps avatars aligned) |
| Leading | 32px avatar circle — initials (or later photo), fill `#E8E8E9`, ink `#1A1B1F` |
| Top | **Sender** (semibold) · **time** (tertiary, trailing) |
| Mid | **Subject** (medium) · optional attachment glyph |
| Bottom | 1–2 line preview (secondary/tertiary) |

- **Replied:** 12×12 curved reply arrow in the status column (Apple Mail pattern). Color = secondary `#6B6F76` on selected white row · tertiary `#9B9BA3` on idle. Forwarded may use a forward arrow in the same slot.  
- Unread: slightly heavier sender/subject (or unread dot).  
- Selected: white fill + hairline (not system blue).  
- Idle: transparent on gray; optional 1px `#E2E2E5` hairline *under* the row (Apple-style), not full card borders on every idle row.

### Toolbar (prefer shadcn column header, not Apple floating pods)

- **Decision:** Reader actions live in the **reading column header** (shadcn/mail pattern), aligned with list “Inbox” header height (~48px). Icon-only 32×32 white + hairline buttons.  
- **Why not Apple floating pods:** Lyra chrome is flat cool gray (no vibrancy/glass/shadow). Floating pill clusters fight that language.  
- **Groups (left → right):** Manage (Archive · Trash · Flag) · Respond (Reply · Forward · Reply all later) — optional 1px divider between groups.  
- **List column head:** title + search icon (same height band). Compose stays in sidebar.  
- No labeled Reply/Archive pills in the reader chrome.

## Icons

- **Style:** 16×16 Lucide-like line icons, stroke ~1.75, rounded caps. Color = tertiary `#9B9BA3` idle / secondary `#6B6F76` default / ink `#1A1B1F` when row is active.
- **Where:** sidebar accounts + folders, Compose, settings nav, reading-pane actions (Reply / Archive / More).
- **Not:** filled brand icons, emoji, colored glyphs in chrome (status dots stay status-only).

## Type

- Instrument Serif — stamp **L** and product wordmark only  
- Instrument Sans (or system UI sans) — everything else  

## Reject

Orange/crimson brand paint · warm bone/beige · black ink rail · purple AI chrome · decorative color.
