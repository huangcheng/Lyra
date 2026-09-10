# Lyra — Sandboxed-iframe Email Rendering, Restored (Design)

Date: 2026-09-10
Status: implemented
Supersedes: docs/superpowers/specs/2026-08-25-mail-inpage-render-design.md

## Why the in-page model broke

The in-page renderer (2026-08-25) had to strip `<style>` blocks and `class`
attributes to protect the app document (global CSS leaks, Tailwind-class
piggyback overlays). Real HTML email depends on exactly those features:
class-based padding rules, media queries, and dark-mode overrides. Users saw
bare preheader text, lost centering, and broken marketing layouts — the
2026-09-09 Xiaomi MiMo invite vs Apple Mail comparison made this undeniable.

A second, independent leak compounded it: ammonia dropped the `<title>` tag
but kept its text, so the document title appeared as bare text at the top of
stored bodies. Fixed at ingest (`title` is now a clean-content tag); existing
rows keep the stray line, new syncs are clean.

## Decision

Render message bodies in a sandboxed `<iframe srcdoc>` again
(`frontend/src/components/mail/mail-body-frame.tsx`):

- `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` —
  no `allow-scripts`, so email JS can never run; same-origin lets the parent
  auto-size the frame and scan for tracking pixels; popups escape so links
  open as normal tabs (DOMPurify also forces `target=_blank`).
- DOMPurify frame profile (`sanitizeEmailHtmlForFrame`) keeps `<style>` and
  `class`; the strict in-page profile stays for compose quotes.
  `FORCE_BODY: true` is required — without it the fragment parser relocates
  a leading `<style>` to the head and it is lost.
- Auto-height: measure `scrollHeight` on load + `ResizeObserver` on the frame
  body; no inner scrollbar, the pane scrolls as before.
- Sheet defaults (white canvas, link color, img max-width, blockquote, hr)
  live in `buildMailBodyDocument` and act as defaults only — author styles
  come later in document order and win ties.

## The 2026-08-25 jank concerns, addressed

- First-paint delay: srcdoc renders immediately; remote images were already
  proxied/allowed by the privacy layer.
- Height jump / clipped box: the frame starts at 0 height and grows once on
  load; `ResizeObserver` covers late-loading images. No fixed-height clip.
- Origin isolation: this model is strictly safer than in-page (scripts
  disabled by sandbox, not just by sanitizer).

## Notes

- `.mail-body` CSS in `index.css` was removed; its rules moved into the
  sheet document.
- Tracking-pixel detection runs inside the frame document and reports via
  callback; the advisory banner is unchanged.
