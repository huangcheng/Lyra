# Lyra — AI Assist Roadmap (Post-v1)

**Date:** 2026-08-21  
**Status:** Intent / roadmap only — not a v1 delivery requirement  
**Approach:** Optional BYOK AI module; features ship in split waves after core sync

---

## Purpose

Record the intent that Lyra may later offer **optional AI assists** (draft/reply, categorize, spam, calendar-from-email) using the user’s own LLM credentials (**BYOK**). Lyra does not operate a hosted LLM. AI stays **off by default** and never blocks v1 completion.

Calendar writes prefer **CalDAV**, with optional **OAuth provider adapters** (Cursor-style integrations) for services that need them (e.g. Google Calendar API, Dida365).

---

## Decisions locked

| Decision | Choice |
|----------|--------|
| Timing | After core mail sync is solid; **no AI in v1** |
| Strategy | Optional AI module + BYOK; OpenAI-compatible and Anthropic dialects |
| First foothold | Document roadmap now; implement features only post-v1 |
| Feature order | BYOK → draft/reply → categorize → spam assist → calendar-from-email |
| Spam automation | User chooses mode (see below) |
| Spam report | Best-effort to sending domain `abuse@` and/or WHOIS abuse contact |
| Mail mutations | Always via IMAP/JMAP; AI does not own protocol |
| Calendar mutations | CalDAV first; optional OAuth adapters behind one “create event” seam |
| Calendar confirm | Suggest-only by default (user confirms before create) |

---

## Phases

### P0 — Core mail (v1)

Sync, search, folders, compose, provider spam folders/flags, CalDAV/CardDAV cache. **No AI settings or LLM calls.**

### P1 — BYOK foundation (post-v1)

- Settings: base URL, API key (encrypted like mail credentials), model, **API dialect**
- Dialects at the LLM client seam:
  - OpenAI **Chat Completions**
  - OpenAI **Responses**
  - **Anthropic Messages**
  - Any Chat Completions–compatible endpoint (e.g. local Ollama / vLLM)
- Connection test; global AI master switch; per-feature flags default **off**
- Privacy notice: enabling a feature may send mail content to the configured endpoint

### P2 — Draft / reply assist

- Suggest draft or reply text in compose
- User always edits and sends; Lyra never auto-sends

### P3 — Categorize

- Suggest labels/folders
- User confirms before apply (stricter auto modes only if added later by explicit settings)

### P4 — Spam assist

Per-user mode:

| Mode | Behavior |
|------|----------|
| Off | No AI spam path |
| Suggest | Propose spam; user confirms |
| Auto → Spam | AI decides; move to Spam via IMAP/JMAP |
| Auto-delete | AI decides; delete/trash via protocol (exact trash vs expunge TBD at implementation) |
| Report | Best-effort report to sending domain `abuse@` and/or WHOIS abuse contact; no delivery guarantee |

### P5 — Calendar-from-email

**Scene:** AI detects a meeting or schedule in a message → proposes a calendar event → user confirms → Lyra creates it on a connected calendar.

**Integrations hub (Cursor-style):**

- Connect calendars via **CalDAV** (URL + credentials) and/or **OAuth** to provider adapters
- **Primary write path:** CalDAV (Apple/iCloud, generic CalDAV; Google when CalDAV works for the account)
- **Optional adapters later:** Google Calendar API, Dida365 / TickTick API, others — same “create event” seam
- OAuth tokens encrypted at rest; per-integration enable/disable

**Default:** suggest-only; never auto-add without confirmation in the first ship of this feature.

Can proceed once CalDAV create is reliable; may run in parallel with P4 once that prerequisite exists.

---

## Architecture (when built)

- Product capability under **`/api/v1`** (client-agnostic; web is a peer).
- Deep **`ai` / `assist` module** behind thin Axum handlers.
- **LLM client seam** with adapters for Chat Completions, Responses, and Anthropic Messages.
- Assist features invoke **existing mail and PIM action seams** (move, delete, label, draft, create calendar event) — never talk to IMAP/JMAP/CalDAV/OAuth providers directly from LLM code.
- **Calendar provider seam:** CalDAV adapter + pluggable OAuth adapters (one interface: create/update/delete event).
- Auto spam paths run as **resumable, auditable jobs** (message id, decision, action, outcome).
- Report mode resolves From/Reply-To domain, attempts `abuse@domain` and WHOIS abuse contact, stores best-effort local outcome.

---

## Privacy & security

- AI **off** by default; no Lyra-hosted model; no prompt telemetry.
- API keys and OAuth tokens encrypted at rest; never logged.
- Send only the minimum content required for the active feature.
- Clear UI copy when content will leave the Lyra instance (LLM endpoint and/or calendar provider).

---

## Explicit non-goals

| Non-goal | Reason |
|----------|--------|
| AI in v1 success criteria | Core sync/search/compose first |
| Hosted Lyra LLM | Conflicts with self-hosted / BYOK positioning |
| Guaranteed abuse-report delivery | External mail/WHOIS is best-effort |
| Replacing provider spam with Rspamd | Separate concern; provider folders remain primary |
| Auto-send of replies | User always controls send |
| Auto-create calendar events without confirm (first ship) | User confirms proposed events |
| Replacing CalDAV with Graph-only calendar | CalDAV remains primary; Graph/vendor APIs are optional adapters |

---

## Relationship to other docs

| Doc | Role |
|-----|------|
| `docs/product/2026-08-20-lyra-v1-product-spec.md` | v1 scope; AI is later; CalDAV/CardDAV in v1 |
| `docs/product/2026-08-20-lyra-multi-client-roadmap.md` | Clients share `/api/v1` including future AI routes |
| `docs/specs/2026-08-20-lyra-engineering-standards.md` | Deep modules, encrypted secrets, thin handlers |
| `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` | IMAP/JMAP remain source of truth for moves/deletes |
| `docs/specs/2026-08-20-lyra-data-model-spec.md` | `calendar_event` cache; CalDAV sync shape |

---

## Non-goals of this roadmap note

- Prompt templates, eval harnesses, or model benchmarks  
- Full OpenAPI for `/api/v1/ai/*` or `/api/v1/integrations/*` (implementation specs later)  
- Exact WHOIS library / abuse email SMTP path (implementation detail)  
- Per-vendor OAuth client registration steps (docs at implementation time)  
