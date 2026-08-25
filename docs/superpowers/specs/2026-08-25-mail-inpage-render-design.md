# Lyra — In-Page Email HTML Rendering (Design)

Date: 2026-08-25
Status: approved by user (pending spec review)
Supersedes: the sandboxed-iframe reader introduced with remote-image proxy M1

## Problem

The reader renders message bodies in a sandboxed `<iframe srcdoc>`. Users
perceive this as slow and janky: iframe first-paint delay, height jump when
the parent measures and resizes the frame, and a clipped fixed-height box
before sizing. Apple Mail / Thunderbird hide the same isolation behind
native pre-warmed views and offline bodies; a web app can't copy that
directly.

## Evidence (verified 2026-08-25 in the user's own logged-in sessions)

| Client | Body render | Class handling | `<style>` blocks |
|--------|-------------|----------------|------------------|
| Fastmail | in-page `div.u-article`, **0 iframes** | all classes stripped | rewritten/emptied (`@scope {}` shell) |
| Outlook web | in-page `<div>`, **0 iframes** | every email class prefixed `x_` | 0 tags; inline styles kept (166 on sample) |
| Apple Mail | WKWebView (separate web process, JS off) | — | — |
| Thunderbird | `<browser>` content process, JS off | — | — |

Two industry camps exist. The in-page camp (Gmail/Outlook/Fastmail) renders
attacker HTML in the app DOM with aggressive sanitization: neutralize
classes, kill `<style>` blocks, keep inline styles.

## Decision

Adopt the in-page model, Fastmail/Outlook-style. The iframe, srcDoc CSP,
and autosize plumbing are removed from the reader.

Accepted trade-off: no origin isolation — a sanitizer zero-day would run
with Lyra's session. Same risk accepted by Gmail/Outlook/Fastmail;
mitigated by double sanitization (ammonia at ingest + DOMPurify at render)
and by the class/style restrictions below, which remove the two practical
in-page attack shapes (Tailwind-class piggyback overlays, global CSS leaks).

## Design

### Render path (`frontend/src/components/mail/mail-display.tsx`)

- Sanitized body HTML renders directly into a `<div className="mail-body">`
  via `dangerouslySetInnerHTML`. No iframe, no shadow DOM.
- The loading skeleton (added 2026-08-25) stays, shown until the body fetch
  resolves; the fade-in now applies to the div.
- `mailHtmlSrcDoc`, `frameHeight`/`frameLoaded`, and the `isDark` threading
  are deleted. Typography moves to scoped CSS (below), so dark mode is
  inherited from the app instead of computed per-frame.

### Render-pass sanitize (DOMPurify, frontend)

Current config plus two in-page-specific rules:

- **Forbid `class` attributes** (Fastmail-style strip). Rationale: Lyra is
  a Tailwind app — an email carrying `class="fixed inset-0 z-50 bg-black/80"`
  would otherwise become a ready-made full-screen overlay using our own
  utilities. (Outlook's `x_` prefixing was considered; stripping is simpler
  and equally effective since `<style>` is forbidden, so no email CSS can
  target classes anyway.)
- **Forbid `<style>` tags.** A `div {…}` / `body {…}` rule in email CSS
  would restyle the app. Inline `style` attributes carry email layout and
  are self-scoping; they stay (unchanged).
- A DOMPurify `afterSanitizeAttributes` hook forces
  `target="_blank" rel="noopener noreferrer"` on all `<a>`.

### Ingest sanitize (ammonia, backend)

Unchanged: keeps `<style>` and inline styles in storage. Stored HTML stays
maximally reusable by any future client; render-side policy is a display
concern, not a storage concern.

### Overlay containment + typography (`frontend/src/index.css`)

New `.mail-body` rules:

- `transform: translateZ(0)` on the wrapper — establishes a containing
  block so `position:fixed` from email inline styles is boxed inside the
  message area (can't overlay app chrome).
- Base typography (moved from the deleted srcDoc stylesheet): system sans
  inherited, `img/table { max-width: 100% }`, `img { height: auto }`,
  `pre/code` monospace + `pre` wrap, styled `blockquote`, `hr`, tightened
  heading line-height, `overflow-wrap: break-word`. Colors use existing
  design tokens so light/dark follow the app theme.

### Out of scope (unchanged)

- Remote-image privacy: backend already rewrites/blocks remote `img` URLs
  when serving a body (`?remote_content=allow`); never depended on iframe
  CSP.
- Offline/eager body fetch (instant open, offline reading): separate
  follow-up; orthogonal to this change.
- CHE-129 (IMAP timeouts): touches the same fetch path, scheduled
  separately.

## Testing

- Frontend: DOMPurify config unit tests — `class` stripped, `<style>`
  dropped, inline `style` kept, links get `target`/`rel`, scripts/handlers
  still removed.
- Manual verification matrix (agent-browser): GitHub PR notification
  (table layout), a newsletter with inline styles, dark mode, and a hostile
  fixture with `class="fixed inset-0 z-50"` + `<style>body{…}</style>` +
  `position:fixed` inline — assert no app restyle and no overlay escape.
- Existing backend sanitize tests unchanged (storage policy untouched).

## Docs

Update `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md` to note the
render layer no longer relies on iframe CSP (privacy enforcement is purely
server-side rewriting).
