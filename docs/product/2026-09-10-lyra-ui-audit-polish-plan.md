# Lyra UI audit & polish plan — 2026-09-10

Full-interface audit of the web client (light + dark, zh + en spot checks), run
against `PRODUCT.md` (quiet, precise, crafted; color means status; hairlines
over shadows) and the frontend-design anti-slop rules. Evidence: source scan of
`frontend/src` (69 tsx files) plus live screenshots of Mail, Calendar,
Contacts, Dashboard, Settings, Login at 1680px in both themes.

This is an audit + plan, not a fix pass. Each finding names its fix path.

## Anti-patterns verdict

**Passes the AI-slop test.** No gradient text, no glassmorphism, no neon-on-dark,
no hero-metric template, no identical icon-card grids, no purple-blue SaaS
palette. The stamp wordmark (Instrument Serif "L" in an ink square) and the
thinking-orbs loading family give the product a recognizable identity.
Backdrop-blur appears only on 3 sticky list headers (purposeful legibility, not
decoration). Neutrals are a disciplined cool zinc; status colors (amber unread/
today, green ok, muted red destructive) hold in both themes.

Two borderline notes: the main text font is Inter (mainstream, but justified —
bilingual en/zh with CJK metric fallback; Inter Tight display + serif brand
mark carry the identity), and the dashboard stat row flirts with the
hero-metric template but stays restrained (24px numerals, no gradient accent).

## Executive summary

- **0 Critical, 3 High, 5 Medium, 3 Low** findings.
- The design *system* is in good shape: tokens, both themes, motion discipline,
  loading language. The gaps are concentrated in **keyboard/focus accessibility**,
  **target sizes & sub-pixel type**, and a few **un-tokenized or un-themed edges**.
- Overall quality: solid B+. The fastest visible wins are cheap (focus rings,
  a token, a type floor).

Top 3:
1. Custom clickable rows (mail conversations, sidebar rows, calendar chips)
   have **no visible keyboard focus** — WCAG 2.4.7 AA failure. [H1]
2. **5 buttons are 20×20px** (`size-5`) — below the WCAG 2.2 AA 24px minimum;
   another ~10 sit at 24–28px against PRODUCT.md's own 44px touch-target rule. [H2]
3. **72 text spots render below 12px**, five of them at 9px — below any
   readability floor, worse for zh glyphs. [H3]

## Detailed findings

### High severity

**H1 — No focus indicator on custom interactive rows.** `components/mail/mail-list.tsx:539`
(conversation row, `role="button" tabIndex={0}`), sidebar account/folder rows
(`components/mail/sidebar-folders.tsx`), calendar event chips
(`components/calendar-page.tsx:488`), contacts rows. None carry
`focus-visible:` styles and `index.css` has no global focus rule, so Tab
navigation is invisible outside ui/ primitives.
Impact: keyboard-only users get lost in the core mail flow.
Standard: WCAG 2.4.7 (AA).
Fix: add a shared focus ring utility (`focus-visible:ring-[2px]
focus-visible:ring-foreground/25 focus-visible:outline-none`) to every
`role="button"` row and custom `<button>`; sweep via `/fixing-accessibility`.

**H2 — Touch/pointer targets below standard.** `size-5` (20px) icon buttons
×7, `h-6 w-6` (24px) ×4, `h-7 w-7` (28px) ×5 (sync popover rows), `icon-sm`
×10 (settings account actions); mini-month day cells are 28px.
Impact: mis-taps on touch hardware; fails WCAG 2.2 AA 2.5.8 (24px floor) for
the 20px set, and misses PRODUCT.md's own 44px touch-target commitment.
Fix: desktop density can stay visual-20px but hit-area must expand
(`relative` + `after:absolute after:-inset-2` or padding) to ≥24px everywhere,
≥40px where `useMediaQuery` reports coarse pointers. `/adapt`.

**H3 — Text below the readability floor.** 72 instances < 12px:
`text-[9px]` ×5 (calendar event overflow badge, attachment badge),
`text-[10.5px]` ×12 (contacts index rail, calendar labels), `text-[10px]` ×8,
`text-[11px]` ×33, `text-[11.5px]` ×14.
Impact: 9–10px is unreadable for dense zh glyphs; metadata at 11px is the
accepted mail-client floor (Apple Mail metadata ≈ 11–12px) and can stay.
Fix: set a hard floor of 11px (badges go to 10px min only for numeric
counters with tabular-nums); re-type the 25 spots at ≤10.5px. `/typeset`.

### Medium severity

**M1 — Hard-coded ink on amber in calendar.** `components/calendar-page.tsx`
×5 use `bg-[var(--unread)]` with literal `text-[#1a1b1f]`. Works today because
`--unread` is identical (#c08532) in both themes, but the pairing is
untracked: any future dark-theme tuning of `--unread` silently breaks contrast.
Fix: add `--on-unread` (light/dark) in `index.css` and swap the literals.
Same treatment for `#c8c9cd` in `components/ui/resizable.tsx:39`. `/normalize`.

**M2 — No mobile story for PIM pages.** Calendar (fixed 232px sidebar),
Contacts (fixed w-80 list), Settings, Dashboard have no `useMediaQuery`/`md:`
handling; only Mail, Compose, and auth adapt. At 390px the calendar sidebar
eats 60% of the screen.
Impact: unusable on phones; the web app is the only client until far-horizon
native apps. Fix: decide v1 scope (desktop-first = document it in PRODUCT.md)
or adapt sidebars into drawers. `/adapt`.

**M3 — 3.1MB uncompressed main JS bundle.** Plate.js compose editor, cmdk,
thinking-orbs engine, XState all load eagerly on first paint.
Impact: slow first load on the low-end hardware self-hosters favor.
Fix: route-level `React.lazy` for Dashboard/Calendar/Contacts/Settings;
dynamic-import Plate in compose; defer assistant widget. `/optimize`.

**M4 — Empty states don't teach.** Reading pane ("选择一封邮件以阅读") and the
no-message state are bare text/icon. PRODUCT wants calm *and* crafted.
Fix: branded empty states — stamp mark, one-line hint, 2–3 keyboard shortcuts
(j/k, ⌘K, c to compose). `/onboard` + `/delight`.

**M5 — Elevation tokens drift from "hairlines over shadows".**
`shadow-md` on popover, `shadow-lg` on dialog (ui primitives), while the brand
rule is hairlines with `shadow-whisper`/`shadow-lift` reserved for true
overlays.
Fix: normalize overlay elevation: popover/dropdown → hairline + `shadow-sm`;
dialog keeps `shadow-lift`. Audit via `/normalize`.

### Low severity

**L1 — "今天" (Today) button renders as a bright pill in dark mode**
(calendar header, screenshot evidence) while its sibling outline buttons stay
dark. Verify variant/background and align. `/polish`.

**L2 — Redundant month landmark at rest.** Month view shows "2026年9月" in both
the h1 and the first sticky in-grid label row when scrolled to top. Consider
fading the in-grid label until the row actually sticks. `/polish`.

**L3 — Unlocalized server folder names** ("Notes", "Outbox") sit next to
localized roles in the sidebar. Map known roles to i18n; keep raw name as
fallback. `/harden`.

## Systemic patterns

- **Focus discipline ends at ui/ primitives.** Every hand-rolled row/button
  needs the same two classes; add an oxlint-friendly convention: custom
  `role="button"` ⇒ must carry `focus-visible:` ring.
- **Pixel literals cluster in new pages** (calendar, contacts). The `11px`
  convention is fine; anything smaller should trip review.
- **Size tokens are ad hoc** (`h-7 w-7`, `size-5`, `size-6`, `icon-sm`).
  Introduce two control sizes — `icon-sm` (24px hit) and `icon-md` (32px hit) —
  and ban raw `size-*` on buttons.

## What's working (keep and replicate)

- Token-driven theming with intentional per-theme palettes
  (`LIST/READER/PANEL/HOV` comments in `index.css`); dark mode verified clean
  on Mail and Calendar.
- thinking-orbs as the single loading language (24 call sites, semantic
  states, reduced-motion + offscreen pausing for free).
- Status-color discipline: amber = unread/today, green = healthy, red =
  destructive; no decorative color anywhere.
- Real semantics where it matters: calendar day cells and mini-month are
  `<button>`s with aria-labels; mail rows are `role="button" tabIndex={0}`.
- Motion tokens (`--ease-out-quart`, stagger caps, reduced-motion guard in
  `index.css`) — restrained and consistent.
- Zero AI-slop tells (no gradients, no glass, no glow, no template grids).

## Plan by priority

**Immediate (one sitting, low risk)**
1. Focus rings on all custom rows/buttons (H1) — shared class + sweep.
2. `--on-unread` token + resizable-handle token (M1).
3. Type floor: bump the 25 spots ≤ 10.5px (H3, badge part).

**Short-term (this sprint)**
4. Hit-area normalization to ≥24px everywhere, 40px on coarse pointers (H2).
5. Branded empty states for reading pane + mail list (M4).
6. Bundle split: lazy routes + dynamic Plate (M3).

**Medium-term (next sprint)**
7. Control-size tokens (`icon-sm`/`icon-md`) + codemod raw `size-*` buttons.
8. Overlay elevation normalization (M5); Today-button dark fix (L1);
   sticky-label redundancy (L2); folder-name i18n mapping (L3).
9. Remaining 11px audit pass with a zh readability check (H3 remainder).

**Long-term (deliberate bets)**
10. Mobile decision for PIM pages: scope statement or drawer adaption (M2).
11. `/delight` pass: one orchestrated entrance per destination page; teaching
    empty states everywhere; unsubscribe/tracking-pixel banner polish.

## Suggested commands for fixes

- `/fixing-accessibility` — H1, H2 (focus, targets)
- `/typeset` — H3 (type floor)
- `/normalize` — M1, M5 (tokens, elevation)
- `/optimize` — M3 (bundle splitting)
- `/onboard` + `/delight` — M4 (empty states)
- `/adapt` — H2 coarse-pointer hit areas, M2 (responsive)
- `/polish` — L1, L2 (calendar details)
- `/harden` — L3 (folder i18n), plus overflow/edge checks after the type bump
