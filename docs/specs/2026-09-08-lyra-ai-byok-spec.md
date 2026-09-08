# Lyra — AI Assist P1 (BYOK) + P2 (Draft/Reply) Spec

**Date:** 2026-09-08
**Status:** Implementing
**Parent:** [AI assist roadmap](../product/2026-08-21-lyra-ai-assist-roadmap.md) (P1 + P2 slice)
**Engineering standards:** [engineering standards](./2026-08-20-lyra-engineering-standards.md)

---

## Scope of this slice

P1 — BYOK foundation: per-user AI provider settings (dialect, base URL, model,
API key encrypted at rest), connection test, master switch, feature flags
(all default off). P2 — draft/reply assist: generate suggested reply or
forward text for one message; the user always edits and sends.

Out of scope (later phases): categorize, spam assist, calendar-from-email,
streaming, tool-calling agents.

## Data model

`ai_settings` (migration `0023_ai_settings`, all three dialects):

| column | type | notes |
|--------|------|-------|
| `user_id` | PK → `lyra_user.id` ON DELETE CASCADE | |
| `enabled` | int/bool, default 0 | master switch |
| `dialect` | text, default `openai_chat` | `openai_chat` \| `openai_responses` \| `anthropic` |
| `base_url` | text, default '' | versioned API root, e.g. `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `model` | text, default '' | e.g. `qwen3-max` |
| `api_key` | text, default '' | DEK-encrypted JSON blob (`crypto::encrypt`), same envelope as mail credentials |
| `features` | text (JSON), default `'{}'` | per-feature flags, e.g. `{"draftReply":true}` — every feature default **off** |
| `updated_at` | timestamp | |

## `ai` module (backend/src/ai/)

Deep module; everything outside talks to these seams:

- `AiSettings` (typed), `load_settings(db, user_id)`, `save_settings(...)` —
  api key never leaves the module in plaintext (handlers see `has_key` only).
- `LlmClient::complete(req)` → `String` — one completion call. Adapters:
  - `openai_chat`: `POST {base}/chat/completions`
  - `openai_responses`: `POST {base}/responses`
  - `anthropic`: `POST {base}/v1/messages` (`x-api-key` + `anthropic-version`)
- `test_connection(db, user_id)` — tiny fixed prompt, returns the reply text.
- `draft_reply(db, user_id, message_id, mode, instruction)` — builds the
  prompt from the stored message (subject, sender, date, body text truncated
  to 8 KB), calls `complete`, returns suggested text. Never mutates mail.

Rules: base URL is used verbatim (documented as the versioned root); 30 s
timeout; typed errors surface to HTTP 400/502 (`ai_not_configured`,
`ai_unreachable`, `ai_error {detail}`); feature disabled → 403-style typed
error; **no logging of key or prompt bodies**.

## HTTP API (client-agnostic `/api/v1`)

| route | behavior |
|-------|----------|
| `GET /settings/ai` | `{enabled, dialect, baseUrl, model, hasKey, features}` |
| `PUT /settings/ai` | any of the above; `apiKey` omitted ⇒ keep existing; dialect validated; blank apiKey clears |
| `POST /settings/ai/test` | runs `test_connection`, `{ok, reply, model}` or typed error |
| `POST /ai/draft` | `{messageId, mode: 'reply'\|'forward', instruction?}` → `{text}` |

## Frontend

- Settings → new **AI** section: master switch, dialect select, base URL,
  model, API key (password input, write-only), Test button with live result,
  per-feature toggles (first: assist reply/forward), privacy notice
  (en/zh): content is sent to the configured endpoint only while a feature
  is on.
- Compose: an assist action (visible only when enabled+configured+feature
  on) fills the editor with the suggestion for reply/forward modes;
  `sourceMessageId` rides on `ComposeDraft`.

## Security & privacy

- Key encrypted with the user's DEK; never returned by any GET; never
  logged; gitleaks-green.
- AI off by default; every LLM call is user-initiated (test button or
  compose assist) — no background calls in this slice.
- Only the minimal context (one message, truncated) is sent.

## Tests

- Unit: dialect request/response shaping (fixture JSON), settings
  roundtrip + has_key semantics (sqlite), prompt builder truncation.
- Live verification: manual against a scratch instance using the user's
  DashScope key (`openai_chat` dialect, compatible-mode endpoint) — key
  supplied at runtime, never committed.
