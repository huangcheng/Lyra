# Lyra Remote-Image Proxy (Anti-Tracking) — Phase Spec

**Date:** 2026-08-23
**Status:** Planned (separate track from the [OpenGPG spec](./2026-08-23-lyra-opengpg-spec.md))
**Scope:** Stop email tracking pixels and sender-side fingerprinting by controlling how remote images in HTML mail are loaded.

---

## Why

HTML mail routinely embeds 1×1 tracking pixels and remote images. Loading them directly leaks to the sender: that/when the mail was opened, client IP, user agent, screen hints, and re-open timing. Lyra already sanitizes stored HTML (`sanitize.rs`) and has SSRF guards (`netsec.rs`), but remote images currently load straight from the sender's host into the app origin.

## Goals

- **Default: no remote fetch happens just because a message is rendered.**
- Proxy remote images through the backend — **senders see only the Lyra server's IP and a generic server user-agent, never the user's device**.
- Cache proxied images so re-opening a message never re-contacts the sender.
- Per-message "load remote content" escape hatch, plus a global mode setting.
- Serve remote images under `'self'` (proxied) or placeholders so the HTML can
  render without contacting sender hosts. Privacy is enforced by **server-side
  rewriting** at message serve time — not by the web reader's iframe CSP
  (in-page render as of 2026-08-25; see `docs/superpowers/specs/2026-08-25-mail-inpage-render-design.md`).
- Work for every API client — rewriting happens at serve time in the backend, not web-only.

## Non-goals (this phase)

- No general web proxying — images only (content-type enforced).
- No link rewriting/unshortening (separate future concern).
- No CSS `background-image` support (sanitizer already strips `style` attributes; document as intentional).
- No blocking of non-image remote resources beyond what the sanitizer already removes.

## Design

### Modes (per-user setting, default `block`)

| Mode | Behavior |
|------|----------|
| `block` | Every remote `http(s)` image src replaced with a neutral placeholder (`<span data-lyra-blocked-img>` with alt text). Banner above the message: **“Remote content was hidden”** with two actions — **“Show remote content”** (this message only, via `?remote_content=allow`) and **“Always show remote content from {sender}”** (adds the sender to the allow-list). |
| `proxy` | Remote srcs rewritten to `/api/v1/proxy/<original-url>`; first render triggers a backend fetch, cached afterwards. |

Setting stored via the existing kv/settings seam; API: `GET/PATCH /api/v1/settings/privacy` → `{ "remote_images": "block" | "proxy", "remote_content_allowlist": ["sales.cn@jetbrains.com", …] }`.

### Per-sender allow-list

- “Always show remote content from {sender}” stores the message’s primary `From` email address (lowercased, exact match) in the kv/settings seam alongside the mode.
- Allow-listed senders **skip blocking only — they do not skip the privacy layer**: once M2 exists their images still load via `/api/v1/proxy/…` in `proxy` mode (server IP/UA only); in M1 (no proxy yet) they load directly, matching Thunderbird’s behavior.
- API: `POST /api/v1/settings/privacy/allow-sender { "sender": "…" }` and `DELETE /api/v1/settings/privacy/allow-sender/{sender}`; both are idempotent and reflected in `GET /api/v1/settings/privacy`.
- Serve-time rewriting resolves allow-list membership from the message row’s `from` address before choosing placeholder vs load.
- Manage/delete entries: Settings → Privacy list (M3).
- i18n (en/zh): banner title, “Show remote content”, “Always show remote content from {sender}”, settings labels.

### Serve-time rewriting

- Single choke point: a new `rewrite_remote_images(html, mode)` applied in `message_response_from_row` / message-serve paths in `sync/http.rs` — after `persist_body_html` sanitization, never at ingest (stored HTML stays pristine except sanitization).
- `<img>` only. Also rewrite `srcset`/`sizes` pairs if present (or drop `srcset` — decision in M1).

### Proxy endpoint `GET /api/v1/proxy/{original-url…}` (decided shape)

The rewritten src keeps the original URL visible in the path, e.g.

```
https://lyra.example.com/api/v1/proxy/https://tracker.example.com/pixel.gif%3Fid%3Dabc
```

Encoding rules at rewrite time (Lyra generates these URLs, so it controls correctness):
- scheme + host + path kept readable (slashes intact);
- `?` → `%3F` and `#` → `%23` so the original query/fragment survive as path data (otherwise the browser would treat them as the proxy request's own query);
- `sig` query param appended: short-lived HMAC signature over the target URL (per-user media secret, ~24 h TTL). Auth is bearer-token only in Lyra and `<img>` cannot send headers, so `sig` is the credential for image requests. The sender only ever sees the Lyra server fetch — no user IP, UA, or session data leaves the server.

Handling:

1. Verify `sig`; reject tampered/expired signatures (404, static placeholder).
2. Compute cache key `sha256(url)`.
3. Cache hit → stream from `data/media-cache/{aa}/{hash}` with `Cache-Control: private, max-age=31536000, immutable`.
4. Cache miss → fetch upstream:
   - Reuse `netsec::filter_public_addrs` (SSRF guard) on every hop incl. redirects (cap redirects at 3).
   - No cookies, no Referer; fixed generic User-Agent (`Lyra/1.0`); 10 s timeout; 10 MB size cap (streaming abort).
   - Accept only `image/*` responses; sniff bytes as a second check.
5. Store to cache, then stream to the client.

Cache eviction: LRU by atime, capped at a configurable size (default 512 MB); eviction is cosmetic (miss just refetches).

### Tracking-pixel heuristics (advisory only)

During rewrite, flag images with explicit dimensions ≤ 4×4 px (`data-lyra-pixel="1"`). After fetch, tiny payloads (≤100 bytes or GIF/PNG ≤4×4) set response header `X-Lyra-Pixel: 1`. Reading pane shows a subtle advisory badge; never auto-blocks.

## Phases

### M1 — Block by default + manual load (no proxy yet)
- `rewrite_remote_images` placeholder mode; `?remote_content=allow` bypass on message endpoints.
- Per-sender allow-list: kv persistence, allow/forbid API, `From`-match in the rewriter.
- Frontend: blocked-content banner in `mail-display.tsx` with the two actions (Thunderbird-style, per-message, not per-app prompt spam); i18n en/zh.
- Optional but cheap here: rewrite `cid:` inline images to the existing attachment download endpoint so inline attachments render (flagged as future work in `sanitize.rs` today).
- CSP note (historical M1): early reader used a sandboxed iframe `img-src`;
  current web UI renders in-page — privacy remains server rewrite + DOMPurify.

### M2 — Proxy + cache
- `media` module: URL encode/decode + `sig` HMAC, per-user media secret, cache store + LRU eviction.
- `GET /api/v1/proxy/{url}` with all fetch guards above; reuse `netsec`.
- `proxy` mode rewriting; setting + `PATCH /api/v1/settings/privacy`.
- Tests: SSRF refusal (private/loopback resolved hosts), redirect guard, non-image content-type rejection, cache-hit never refetches (mock upstream with hit counter), bad/expired `sig` → 404 placeholder, query-string round-trip (`?`/`#` survive the path encoding).

### M3 — Refinements
- Pixel heuristics + UI badge. **done** (CHE-60)
- Allow-list management UI: Settings → Privacy shows stored senders with remove buttons; optional domain-level entries (`@jetbrains.com`) — decide here.
- Cache stats in settings page (size, clear-cache button).

## Schema / storage

- No new tables for M1–M2 (settings via existing kv seam; cache on disk).
- New migration only if we later persist per-sender allow-lists in SQL (prefer kv until then).

## Security rules

- Proxy endpoint must never become an open relay: `sig` required (no valid signature → 404), image content-type enforced on response, SSRF filter on all hops, size/timeout caps, no request header forwarding.
- No logging of full proxy URLs (log cache-key hash only); media secret rotates on credential reset.
- Upstream fetch errors return a static placeholder image — no error text that oracles upstream state.
- The allow-list matches `From`, which is **spoofable**: it is a convenience control, not a security boundary. Allow-listed content still passes sanitization, and (from M2) still loads via the proxy.

## Resolved decisions

1. **Endpoint shape:** path-based `/api/v1/proxy/<original-url>` with short-lived `sig` param (bearer-only auth can't ride on `<img>` requests). Senders see only the Lyra server, never the user's client. ✔

## Open questions

1. Should `proxy` mode also apply to links' favicons/previews later, or stay images-only forever? (Leaning: images-only.)
2. Strip known tracking query params (`utm_*`, `mc_*`, …) before hashing the cache key — small win, mild cache-coherence risk; decide in M2.
3. Interaction with OpenGPG-encrypted mail: decrypted bodies (OpenGPG spec P2) must pass through the same rewrite — single choke point makes this automatic if rewrite lives in the message-response builder.

## Verification

- Unit tests for rewriting (both modes, `srcset`, `cid:`), token roundtrip, cache key stability.
- Integration test with a mock HTTP server: assert zero upstream hits when `block`, exactly one hit across repeated renders in `proxy`.
- Manual check with a real tracked newsletter (e.g. typical marketing mail) in both modes.
- `make check` green.
