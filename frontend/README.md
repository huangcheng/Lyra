# Lyra frontend

Vite + React + TanStack Router + shadcn mail UI. Talks only to `/api/v1`
(typed `api()` client). English + Chinese via `src/i18n/`.

## Dev

```bash
# Backend must be running on :3000 (see ../backend/README.md)
npm install
npm run dev          # http://127.0.0.1:5173 — Vite proxies /api
npm test             # vitest
npm run check        # oxlint + tsc
npm run build        # production bundle → dist/
```

## Layout

```
src/
  components/     ← mail shell, settings, dashboard, ui primitives
  stores/         ← Zustand (mail data, UI)
  machines/       ← XState (auth / OpenGPG unlock)
  lib/            ← /api/v1 client, mappers, confirm dialog helper
  rxjs/           ← SSE sync event stream
  i18n/           ← en + zh
```

Destructive confirms (trash, delete account/key) use the shared in-app
`ConfirmDialogHost` — not `window.confirm`.

Design context: repo-root `PRODUCT.md`, `DESIGN.md`, and
`docs/superpowers/specs/2026-08-24-lyra-redesign-v2-design.md`.
