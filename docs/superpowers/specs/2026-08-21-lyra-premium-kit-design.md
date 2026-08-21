# Lyra — Premium kit (crafted quiet + soft depth)

**Date:** 2026-08-21  
**Status:** Draft — pending user review  
**Amends:** `docs/specs/2026-08-21-lyra-ui-design.md` (separation / shadow rules)  
**Out of scope:** Implementation, Remotion (video only), anime.js for shell chrome

---

## Goal

Make auth + mail feel **richer and more premium** without leaving cool utility gray.

**Feel:** Crafted quiet (A) + soft depth (C).  
**Depth budget:** Hairline + tone as base; **whisper elevation** on a short allowlist. No glass/blur chrome.  
**Apply:** Both auth and mail, lightly (same language, small delta each).  
**Approach:** Micro-system (one kit everywhere) — not surface drama, not motion-only.

---

## Motion library

**Use Motion** (`motion` / Framer Motion) for shell UI.

- State-driven chrome: row select, menu/compose enter-exit, auth mount, `whileTap` on controls.
- Gate all transitions with `prefers-reduced-motion: reduce` → instant state, no enter anims.
- **Not Remotion** for product UI (Remotion = video/compositions only, e.g. future promo).
- **Not anime.js** for default shell chrome (prefer Motion’s declarative enter/exit). anime.js only if a later one-off SVG/timeline sequence (e.g. stamp draw) fights Motion.

**Timing:** 120–180ms, ease-out. No continuous/ambient loops.

| Surface | Motion |
|---------|--------|
| Auth card mount | opacity 0→1, `y: 8→0` ~160ms |
| Auth stamp (optional) | one-shot scale `0.96→1` with card |
| Selected list row | background + shadow crossfade ~120ms |
| Toolbar / CTA press | `whileTap` scale ~0.96–0.98 + fill `#E8E8E9` |
| Menus / compose | opacity + `y: 4→0` ~150ms; `AnimatePresence` on exit |
| Splitter drag | follow pointer; optional short ease on release — no spring thrash |

---

## Elevation

**Token `shadow-whisper`:**

```text
0 1px 2px rgba(26, 27, 31, 0.04),
0 0 0 1px rgba(226, 226, 229, 0.8)
```

(Dark mode later: same structure, slightly higher shadow opacity within cool gray invert.)

**Allowlist (only):**

- Selected list row (white card)
- Auth surface card
- Menus / popovers / compose sheet

**Denylist:**

- Sidebar, idle list rows, icon/text buttons at rest
- Reader pane as a whole
- Splitters
- Section headers

**Press / hover (no new hues):**

- Controls: white → `#E8E8E9`; optional 1px settle on press
- Splitter rule: `#E2E2E5` → `#C8C9CD` on hover; cursor `col-resize`

This **replaces** the blanket “no drop shadows” rule in the UI design language with: *no shadows except `shadow-whisper` on the allowlist.*

---

## Status density (mail list)

Keep Apple-inspired rich rows. Fixed **12px** leading status column (avatars stay aligned):

| State | Glyph / treatment |
|-------|-------------------|
| Replied | 12×12 reply arrow — `#6B6F76` selected · `#9B9BA3` idle |
| Forwarded | Forward arrow in same slot |
| Neither | Empty 12px slot |
| Unread | Semibold sender (+ optional 6px ink dot) |
| Flag / attachment | Existing row meta (flag / paperclip) — status-only colors if needed |

---

## Surfaces (light touch both)

### Auth

- Card: white + hairline + `shadow-whisper`
- Stamp + serif L / Lyra wordmark unchanged; tagline tertiary
- Inputs: white + rule (unchanged)
- CTA: white pill; hover/press → gray + Motion `whileTap`
- **Atmosphere:** **Two columns** — white form column (~560) | cool-gray panel with [COBE](https://cobe.vercel.app/) globe. No floating card overlay. See `docs/specs/2026-08-21-lyra-auth-gate.md`.

### Mail

- Selected row: white + hairline + `shadow-whisper`
- Reader: tone `#F9F9FA` only — no pane-level shadow
- Toolbar: icon-only column header; whisper press feedback
- Splitters: 5px hit · 1px rule · hover darken (as existing splitter spec)

---

## Success criteria

- Still reads as Linear-like cool gray at a glance (no brand paint, no glass).
- Selected mail and auth card feel slightly lifted vs chrome.
- Motion is felt in use, not decorative in screenshots.
- Reduced-motion users get the same hierarchy without animation.

---

## Non-goals

- Multi-theme packs, accent chrome, heavy shadows, skeuomorphism
- Remotion in the app shell
- Redesigning information architecture or adding features beyond presentation
