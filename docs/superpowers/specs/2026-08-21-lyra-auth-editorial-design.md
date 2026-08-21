# Lyra — Auth Gate (Editorial Stack)

**Date:** 2026-08-21  
**Status:** Accepted  
**Supersedes atmosphere section of:** `docs/specs/2026-08-21-lyra-auth-gate.md` (two-column + COBE)

---

## Goal

Unauthenticated visitors see a **single-column** auth gate. No empty second panel. Interest comes from stamp, type, and form craft — not a globe.

## Atmosphere

| Element | Treatment |
|---------|-----------|
| Viewport | Quiet cool gray `#F0F0F2` — no watermark, no globe, no grain theatre |
| Form | Centered ~21rem; craft is in the fields, not the page chrome |
| Brand | Stamp + wordmark **in a row** |
| Fields | Rule/underline rows (no boxed inputs); sentence-case labels; ink rule on focus |
| CTA | Ink hairline; hover fills ink (invert) — not Material filled default |
| Motion | Short stagger; honor `prefers-reduced-motion` |
| Anti-patterns | No Material Design; no floating card; no decorative watermark |

## Favicon

Site icon = postage stamp mark (same SVG path as `StampMark`). SVG favicon in `frontend/public/`.

## Unchanged

Routing, bootstrap / login / TOTP flows, cool-gray tokens, no marketing landing, no Voyah.

## Acceptance

- [ ] No two-column layout or COBE on `/login`
- [ ] Form reads as a dense centered stack on gray field
- [ ] Favicon is the stamp mark
- [ ] Flows still work (bootstrap, login, TOTP)
