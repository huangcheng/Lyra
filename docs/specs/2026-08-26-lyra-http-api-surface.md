# Lyra — HTTP API surface

**Date:** 2026-08-26  
**Status:** Active  
**Companion:** OpenAPI contract at [`docs/openapi/api-v1.yaml`](../openapi/api-v1.yaml); engineering summary in [`2026-08-20-lyra-engineering-standards.md`](2026-08-20-lyra-engineering-standards.md).

---

## Versioning

| Prefix | Purpose |
|--------|---------|
| `/health`, `/version` | Unversioned ops probes only. No product data, no auth. |
| `/api/v1/...` | All product capability: auth, mail, sync, accounts, settings, OpenGPG, OAuth, etc. |

Breaking changes ship under **`/api/v2/...`** (see [v2 seam policy](#v2-seam-policy) below). `/api/v1` stays available until clients migrate.

---

## Web client boundary

The React app talks to the backend **only** through `/api/v1`, via the typed client in `frontend/src/lib/api-client.ts`:

- All JSON requests go through `api()` / `apiStream()`, which prefix paths with `/api/v1` and inject `Authorization: Bearer …` when `auth` is true (default).
- The web UI does **not** call unversioned routes except indirectly (e.g. the dev server or deploy may hit `/health` for readiness; the SPA itself does not use `/health` or `/version` for product flows).
- Login, bootstrap, and auth status use `{ auth: false }` so no bearer is sent.

Non-browser clients should use the same `/api/v1` contract and bearer sessions — no web-only shortcuts on the server.

---

## Authentication

Lyra login returns a bearer token. Protected routes require:

```http
Authorization: Bearer <token>
```

Session expiry and missing/invalid tokens return **401** with `{ "error": "…", "code": "unauthorized" }`. Wrong password / bad TOTP on login stay **401** with descriptive `error` text (handled client-side without clearing the session).

See [`docs/openapi/api-v1.yaml`](../openapi/api-v1.yaml) (`securitySchemes.bearerAuth`) and `backend/README.md` for curl examples.

---

## Error envelope

Failed `/api/v1` JSON responses use a stable shape:

```json
{ "error": "human-readable message", "code": "optional_machine_code" }
```

- **`error`** — always present; safe to show in UI or logs (no secrets).
- **`code`** — optional; stable snake_case identifier (`not_found`, `bad_request`, `unauthorized`, `internal_error`, …). Clients may branch on `code`; ignore unknown values.

User-facing copy for i18n still lives in clients; the API returns English diagnostic strings.

---

## OAuth callback (Microsoft)

`GET /api/v1/oauth/microsoft/callback` normally **302 redirects** to `/settings?section=accounts&oauth=…&detail=…` for browser flows.

When the client sends `Accept: application/json`, the same outcomes return JSON instead of a redirect:

```json
{ "status": "ok", "detail": "connected" }
```

or on denial:

```json
{ "status": "error", "detail": "oauth_denied" }
```

Hard failures (invalid state, token exchange, etc.) still use the standard `{ "error", "code"? }` envelope.

---

## v2 seam policy

`/api/v2` is reserved for **breaking** HTTP contract changes. v1 remains the default surface for v1 product scope.

| Change type | Where it goes |
|-------------|----------------|
| Additive fields, new routes, optional query params | `/api/v1` (backward compatible) |
| Renamed/removed fields, semantic changes, incompatible auth | New route under `/api/v2/...` |
| Bug fixes that restore spec/intent | `/api/v1` (same path, corrected behavior) |

Implementation rules:

1. **Parallel trees** — v2 handlers live beside v1 (`/api/v2/...` routes); share domain modules behind seams; do not fork business logic.
2. **OpenAPI** — add `docs/openapi/api-v2.yaml` when v2 ships; keep `api-v1.yaml` frozen except doc fixes.
3. **Clients** — web and future native clients pin a version prefix; migration is explicit (feature flag or app release), not silent.
4. **Sunset** — deprecate v1 only after documented client migration; no forced removal in patch releases.

Non-goals for v2 in the far horizon: duplicating protocol adapters (IMAP/JMAP stay internal); versioning mail-server wire protocols.

---

## When this doc changes

Update this file and pointers in `AGENTS.md` when:

- The web client’s API entry point or unversioned route policy changes  
- Error envelope or auth scheme changes  
- v2 routes ship or v1 sunset is planned  
- OpenAPI location or major route groups change  
