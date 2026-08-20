# Lyra — Multi-Client Roadmap (Far Horizon)

**Date:** 2026-08-20  
**Status:** Intent / roadmap only — not a v1 delivery requirement  
**Approach:** API-as-product (versioned HTTP API first; native clients much later)

---

## Purpose

Record the long-term intent that Lyra’s backend is a **client-agnostic** service. The React web app is the v1 UI and remains a **peer** among future native clients — not a privileged front-end.

Native apps are **explicitly out of v1**. This document does not change v1 success criteria.

---

## Decisions locked

| Decision | Choice |
|----------|--------|
| Timing | Far horizon — after v1 is solid |
| Strategy | API-as-product: OpenAPI + versioned REST; web is just another client |
| Client equality | Web and native are equal peers on the same API |
| Delivery order | **P0 API** → **P1 desktop** → **P2 mobile** |
| Desktop (P1) | macOS **SwiftUI**; Windows / Linux **Qt** |
| Mobile (P2) | iOS **SwiftUI**; Android **Kotlin** (official UI / Compose Multiplatform) |

---

## Phases

### P0 — API-as-product (during v1)

Enforce now while shipping the web client:

- Public UI capability only through **`/api/v1/...`** (no web-only backend shortcuts).
- **OpenAPI** describes the contract; Axum handlers stay thin.
- **Auth** usable by non-browser clients (token-based session as designed for Lyra login).
- **REST** for list/read/mutate; **SSE** (or a versioned equivalent) for live sync events — see sync spec.
- Stable JSON error shapes; API returns data, not locale UI strings (i18n stays in each client).
- Breaking changes → `/api/v2/...`; keep v1 until clients migrate.

### P1 — Desktop peers (after v1)

- macOS SwiftUI and Windows/Linux Qt as equal clients to web.
- Same auth, resources, and event channel.
- Do not start until the P0 contract is stable enough to version.

### P2 — Mobile peers (after desktop)

- iOS SwiftUI and Android (Kotlin / Compose Multiplatform).
- Same API; no mobile-only backend surface.
- Do not start until at least one P1 desktop client has proven the API on a non-web stack.

### P3 — Optional polish (later still)

- Generated client SDKs from OpenAPI, deeper OS integration (notifications, keychain, share targets) — only if needed.

---

## Deferred (no work in v1)

- Native app scaffolds, store/package distribution  
- Generated Swift / Kotlin / C++ SDKs  
- Push notifications and OS mail integrations  
- Any change that makes native apps required to complete v1  

**When P1+ starts:** prefer monorepo packages that depend only on the published OpenAPI contract (e.g. `clients/macos`, `clients/qt`, …), not on `frontend/` React internals.

---

## Relationship to v1

| Doc | Role |
|-----|------|
| `docs/product/2026-08-20-lyra-v1-product-spec.md` | v1 scope; native apps remain non-goals |
| This file | Far-horizon multi-client order and P0 API discipline |
| `docs/specs/2026-08-20-lyra-engineering-standards.md` | Enforce client-agnostic HTTP API rules in day-to-day backend work |
| `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` | Sync events (SSE) alongside REST |

---

## Non-goals of this roadmap note

- Full REST resource catalogue (belongs in a future API spec when handlers exist)  
- Native UI design or platform HIG details  
- Choosing Qt widgets vs QML, or Android View vs Compose, beyond the stack intent above  
