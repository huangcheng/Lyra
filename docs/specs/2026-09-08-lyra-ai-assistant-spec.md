# Lyra — AI Assistant Suite Spec (P3-lite → agent tier)

**Date:** 2026-09-08
**Status:** Implemented (phases A–E)
**Parent:** [AI assist roadmap](../product/2026-08-21-lyra-ai-assist-roadmap.md)
**Foundation:** [BYOK + draft/reply spec](./2026-09-08-lyra-ai-byok-spec.md)

---

## What shipped

One assistant, five capabilities, all behind the BYOK foundation and
per-feature flags (default off):

| Phase | Capability | Surface |
|---|---|---|
| A | Chat with mail search | Floating bubble → dialog (`ai_chat_message` history, migration 0024); `search_mail` tool |
| B | Read + navigate | `read_mail` (full message), `list_folders` tools |
| C | Spam assist | `ai_settings.spam_mode` (migration 0025): `suggest` (reader button → verdict banner) / `auto` (post-sync AI pass, capped 10/pass, `ai_spam`/`ai_clean` verdict stamps, filed through the engine's move seam) |
| D | Calendar-from-email | `POST /ai/calendar/suggest` → event proposal (RFC3339 / all-day forms); reader button → editable confirm dialog → existing CalDAV create |
| E | Act on mail | `propose_draft` / `propose_move` tools → **confirm-first cards** in the panel; nothing executes server-side |

Deferred (documented): spam `auto_delete` + `report` (need the audit
surface first), streaming.

## Tool protocol

`LlmClient::chat()` carries multi-turn history, tool specs, and replayed
tool exchanges natively per dialect: Chat Completions `tool_calls`,
Responses `function_call`, Anthropic `tool_use`. Max 3 tool rounds per
turn, then a forced no-tools answer. Tool failures return error JSON to
the model (it apologizes); only infrastructure errors fail the turn.
Tool arguments accept snake_case/camelCase/bare-id — models drift.

## Safety invariants

- Every mutation is a **proposal card**; the user's click runs existing
  endpoints (compose dialog = draft confirmation; `actOnMessages` for moves)
- The assistant cannot send mail, cannot touch IMAP/JMAP directly
- Auto spam stamps verdicts first (never re-judges on move failure) and is
  independent of the heuristic engine; sender lists still outrank it
- Suggestions never write calendars; creation goes through the CalDAV seam
- API keys DEK-encrypted, never returned; no prompt/key logging; LLM calls
  are user-initigated (auto-spam excepted, capped + audited via verdicts)

## Endpoints

`GET/POST/DELETE /api/v1/ai/chat` (+`actions[]` proposals),
`POST /ai/spam/suggest`, `POST /ai/calendar/suggest` — schemas in
`docs/openapi/api-v1.yaml`. Frontend: `assistant-widget.tsx`,
`ai-event-dialog.tsx`, reader toolbar buttons; feature flags + spam mode in
Settings → AI.

## Verified

Live against DashScope qwen3-max: Chinese invoice search + amounts via
read chaining, context summarize, suggest verdicts both ways (98% spam /
95% clean), auto pass (`ai_spam_live` ignored test), calendar extraction
with timezone ("本周四下午3点" → `2026-09-10T15:00:00+08:00`), draft +
move proposals round-tripping. CJK search fixed along the way (FTS
unicode61 indexes whole runs; non-ASCII queries now take the LIKE path).
