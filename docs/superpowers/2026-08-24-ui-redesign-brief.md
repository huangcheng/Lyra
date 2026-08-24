# UI Redesign Brief — handoff for ardot-capable session

Date: 2026-08-24

## Task (from the user)

The current Lyra web UI is "extremely rough" — redesign it **completely** (new design direction, new design doc/plan), and present the design in Ardot via the `ardot-remote` MCP server (file: https://ardot.tencent.com/file/716978471157674, node 2:680).

"Remove all old design" means: do not build on the current shadcn-mail look; the new design replaces it. Old design docs to supersede: `docs/specs/2026-08-21-lyra-ui-design.md`, `docs/specs/2026-08-21-lyra-ardot-review.md`, `docs/superpowers/plans/2026-08-21-lyra-ui-shell.md`.

## Product context

- Lyra is a **self-hosted mail client** (JMAP preferred, IMAP fallback), web UI first; `/api/v1` is the surface.
- Product truth: `docs/product/2026-08-20-lyra-v1-product-spec.md`; engineering standards: `docs/specs/2026-08-20-lyra-engineering-standards.md`.
- Pages: login/bootstrap/TOTP, three-pane mail (nav / list / reader), compose dialog, contacts, calendar, settings (session, preferences, privacy, security, mail accounts).
- i18n: en + zh. Dark mode exists (light/dark/system, `dark` class on `<html>`).
- Stack (locked): React 19, TanStack Router, Tailwind 4, shadcn/ui, Zustand, lucide-react, Geist Variable font. Follow `AGENTS.md`.

## Current state (as of 2026-08-24, main @ 949b497)

- Stock shadcn mail three-pane + indigo accent (`--primary: #4f46e5` light / `#a5b4fc` dark), star wordmark, shared `EmptyState`, rebuilt calendar grid.
- Frontend checks: `cd frontend && npm run check` (tsc + oxlint + prettier). App runs in Docker at `http://127.0.0.1:3000` (`docker compose up -d --build lyra`). Login: cheng / Lyra@2026.
- Browser review tooling: `agent-browser` CLI works (accessibility snapshots + screenshots).

## Prior design direction (historical, user accepted then moved on)

`docs/specs/2026-08-21-lyra-ui-design.md`: "cool utility gray" — Linear-style white buttons with hairlines, cool gray chrome `#F0F0F2`, charcoal ink `#1A1B1F`, color only for status, sawtooth postage-stamp mark with serif L, thin line icons. Rejected: brand paint, warm beige, black rails, purple AI chrome.

## Process expectations

- Use the superpowers brainstorming → spec (`docs/superpowers/specs/YYYY-MM-DD-*-design.md`) → plan (`docs/superpowers/plans/`) → execution flow.
- Present visual directions in Ardot early (user reviews there), then write the spec, get approval, then implement.
