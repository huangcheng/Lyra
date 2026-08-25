# In-Page Email HTML Rendering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sandboxed-iframe email reader with in-page rendering of sanitized HTML (Outlook/Fastmail model), per `docs/superpowers/specs/2026-08-25-mail-inpage-render-design.md`.

**Architecture:** Sanitized body HTML renders into a `<div className="mail-body">` via `dangerouslySetInnerHTML`. DOMPurify render config forbids `class` attributes and `<style>` tags (Tailwind-piggyback + global-CSS-leak defenses), keeps inline styles, and forces `target="_blank" rel="noopener noreferrer"` on links. `.mail-body` in `index.css` provides `translateZ(0)` overlay containment and base typography from theme tokens. Backend storage sanitization (ammonia) is untouched.

**Tech Stack:** React 19, Tailwind 4, DOMPurify (already a dependency), vitest + jsdom (new devDependencies for the sanitize unit tests).

---

### Task 1: Extract + harden sanitize module with unit tests

**Files:**
- Create: `frontend/src/lib/sanitize-mail.ts`
- Create: `frontend/src/lib/sanitize-mail.test.ts`
- Modify: `frontend/package.json` (devDeps + test script)

- [ ] **Step 1: Add vitest + jsdom and a test script**

Run: `cd frontend && npm install -D vitest jsdom`

In `frontend/package.json` `"scripts"`, add:

```json
"test": "vitest run"
```

- [ ] **Step 2: Write the failing tests**

Create `frontend/src/lib/sanitize-mail.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { sanitizeEmailHtml } from './sanitize-mail';

describe('sanitizeEmailHtml', () => {
  it('strips class attributes (Tailwind-piggyback defense)', () => {
    const out = sanitizeEmailHtml(
      '<div class="fixed inset-0 z-50 bg-black/80">overlay</div>',
    );
    expect(out).not.toContain('class=');
    expect(out).toContain('overlay');
  });

  it('drops <style> tags but keeps inline styles', () => {
    const out = sanitizeEmailHtml(
      '<style>body{background:red}</style><p style="color:red">x</p>',
    );
    expect(out).not.toContain('<style');
    expect(out).not.toContain('background:red');
    expect(out).toContain('style="color:red"');
  });

  it('forces target and rel on links', () => {
    const out = sanitizeEmailHtml('<a href="https://example.com">go</a>');
    expect(out).toContain('target="_blank"');
    expect(out).toContain('rel="noopener noreferrer"');
  });

  it('still strips scripts, event handlers, and javascript: URLs', () => {
    const out = sanitizeEmailHtml(
      '<script>alert(1)</script><img src="https://x/y.png" onerror="alert(2)"><a href="javascript:alert(3)">j</a>',
    );
    expect(out).not.toContain('<script');
    expect(out.toLowerCase()).not.toContain('onerror');
    expect(out.toLowerCase()).not.toContain('javascript:');
    expect(out).not.toContain('alert(');
  });

  it('keeps cid: image references', () => {
    const out = sanitizeEmailHtml('<img src="cid:part1@example">');
    expect(out).toContain('src="cid:part1@example"');
  });
});
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd frontend && npm test`
Expected: FAIL — `Cannot find module './sanitize-mail'`

- [ ] **Step 4: Implement the module**

Create `frontend/src/lib/sanitize-mail.ts`:

```ts
/**
 * Sanitize attacker-controlled email HTML before in-page rendering.
 * Defense in depth: the backend sanitizes at ingest; this guards the render
 * path against legacy rows and other sources.
 *
 * In-page-specific rules (the rendered HTML shares the app's DOM):
 * - `class` attributes are stripped: Lyra is a Tailwind app, so an email
 *   carrying `class="fixed inset-0 z-50 …"` would become a ready-made
 *   overlay using our own utilities (Fastmail strips classes the same way).
 * - `<style>` tags are dropped: email CSS selectors would otherwise restyle
 *   the app. Inline `style` attributes are self-scoping and carry email
 *   layout, so they stay.
 * - All links open in a new tab with noopener/noreferrer.
 */
import DOMPurify from 'dompurify';

let hooked = false;

function ensureHook() {
  if (hooked) return;
  hooked = true;
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    node.removeAttribute('class');
    if (node.tagName === 'A') {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
}

export function sanitizeEmailHtml(html: string): string {
  ensureHook();
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ['iframe', 'object', 'embed', 'form', 'meta', 'link', 'base', 'style'],
  });
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd frontend && npm test`
Expected: 5 passed

- [ ] **Step 6: Commit**

```bash
git add frontend/package.json frontend/package-lock.json frontend/src/lib/sanitize-mail.ts frontend/src/lib/sanitize-mail.test.ts
git commit -m "feat(mail): sanitize module for in-page rendering (strip class/style, safe links)"
```

---

### Task 2: `.mail-body` containment + typography in index.css

**Files:**
- Modify: `frontend/src/index.css` (append)

- [ ] **Step 1: Append the styles**

Append to `frontend/src/index.css`:

```css
/* In-page rendered email bodies (see sanitize-mail.ts). */
.mail-body {
  /* Containing block: position:fixed in email inline styles is boxed here,
     so email CSS cannot overlay app chrome. */
  transform: translateZ(0);
  position: relative;
  font-size: 14px;
  line-height: 1.55;
  overflow-wrap: break-word;
  white-space: normal;
}

.mail-body img {
  max-width: 100%;
  height: auto;
}

.mail-body table {
  max-width: 100%;
}

.mail-body pre,
.mail-body code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.92em;
}

.mail-body pre {
  white-space: pre-wrap;
}

.mail-body blockquote {
  margin: 0;
  padding-left: 0.8em;
  border-left: 3px solid var(--border);
  color: var(--muted-foreground);
}

.mail-body h1,
.mail-body h2,
.mail-body h3,
.mail-body h4 {
  line-height: 1.3;
}

.mail-body hr {
  border: 0;
  border-top: 1px solid var(--border);
}
```

- [ ] **Step 2: Verify formatting**

Run: `cd frontend && npm run format:check` (or `npm run format` if it complains)
Expected: pass

- [ ] **Step 3: Commit**

```bash
git add frontend/src/index.css
git commit -m "feat(mail): .mail-body containment and base typography"
```

---

### Task 3: Replace the iframe with in-page rendering in mail-display.tsx

**Files:**
- Modify: `frontend/src/components/mail/mail-display.tsx`

- [ ] **Step 1: Delete the old sanitize + srcDoc helpers**

Delete `sanitizeEmailHtml` and `mailHtmlSrcDoc` (currently lines 49–93) and the `DOMPurify` import. Add at the import block:

```ts
import { sanitizeEmailHtml } from '@/lib/sanitize-mail';
```

- [ ] **Step 2: Remove iframe state and dark-mode threading**

Delete these declarations and every use:

```ts
const [frameHeight, setFrameHeight] = useState(192);
const [frameLoaded, setFrameLoaded] = useState(false);
const theme = useUIStore((s) => s.theme);
const isDark =
  theme === 'dark' ||
  (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
```

In the `useEffect` on `selectedMessageId`, remove the `setFrameHeight(192);` and `setFrameLoaded(false);` lines (keep `autoMarkedIdRef.current = null;` and `setAllowRemoteContent(false);`).

- [ ] **Step 3: Replace the iframe JSX with the in-page div**

Replace the entire `mail.bodyHtml ? (<div className="relative">…skeleton…<iframe …/></div>) : (` branch with:

```tsx
) : mail.bodyHtml ? (
  <div
    className="mail-body"
    // Sanitized by sanitizeEmailHtml (DOMPurify): no class attrs, no
    // <style>, no scripts/handlers, links forced to _blank+noopener.
    dangerouslySetInnerHTML={{ __html: sanitizeEmailHtml(mail.bodyHtml) }}
  />
) : (
```

Note: the parent container keeps `flex-1 overflow-auto p-4 text-sm`; remove `whitespace-pre-wrap` from that container's className so HTML email doesn't inherit pre-wrap (the plain-text fallback still wraps via `.mail-body`-less default — check the fallback branch still renders `(mail.bodyText ?? mail.snippet)`; wrap it in `<div className="whitespace-pre-wrap">…</div>` so plain text keeps its formatting).

- [ ] **Step 3b: Skeleton while the body is fetching**

Per the spec, keep a loading skeleton for the in-flight body fetch. The reader shows it when the message has no body content yet and no error. Insert before the `loadError` ternary's `mail.bodyHtml` branch logic so the chain becomes:

```tsx
{loadError ? (
  <p className="text-destructive">{loadError}</p>
) : mail.bodyHtml ? (
  <div
    className="mail-body"
    dangerouslySetInnerHTML={{ __html: sanitizeEmailHtml(mail.bodyHtml) }}
  />
) : mail.bodyText ? (
  <div className="whitespace-pre-wrap">{mail.bodyText}</div>
) : (
  <div className="space-y-3 py-1" aria-hidden>
    <div className="h-4 w-2/3 animate-pulse rounded bg-muted" />
    <div className="h-4 w-full animate-pulse rounded bg-muted" />
    <div className="h-4 w-5/6 animate-pulse rounded bg-muted" />
    <div className="h-32 w-full animate-pulse rounded bg-muted" />
  </div>
)}
```

- [ ] **Step 4: Typecheck, lint, format, unit tests**

Run: `cd frontend && npm run check && npm test`
Expected: all pass (0 errors; the 14 pre-existing oxlint warnings may remain)

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/mail/mail-display.tsx
git commit -m "feat(mail): render email bodies in-page (drop reader iframe)"
```

---

### Task 4: Docs + live verification

**Files:**
- Modify: `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md`

- [ ] **Step 1: Update the privacy spec note**

After line 91 (`- CSP tightened: rendered mail iframe/container …`), add:

```markdown
> Note (2026-08-25): the reader renders sanitized HTML in-page (no iframe);
> remote-image blocking is enforced purely by server-side URL rewriting when
> the body is served (`?remote_content=allow` vs default). See
> `docs/superpowers/specs/2026-08-25-mail-inpage-render-design.md`.
```

- [ ] **Step 2: Commit docs**

```bash
git add docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md
git commit -m "docs: note in-page render in remote-image proxy spec"
```

- [ ] **Step 3: Rebuild and verify in the browser**

Run: `docker compose up -d --build` then `agent-browser` against http://127.0.0.1:3000 (login cheng / Lyra@2026):

1. Open the GitHub CI notification email — table layout renders, no iframe, instant paint, links open in a new tab.
2. Toggle dark mode — email text/blockquote colors follow the theme.
3. Inject the hostile fixture (class overlay + `<style>body{…}</style>` + `position:fixed` inline) as a temp message row like previous render tests; confirm no app restyle and no overlay; delete the row after.

Expected: screenshots under `/tmp/lyra-walkthrough/` showing correct rendering; hostile fixture neutralized.

- [ ] **Step 4: Final commit + push**

```bash
git push origin main
```
