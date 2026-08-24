> **Superseded 2026-08-24** by `docs/superpowers/specs/2026-08-24-lyra-redesign-v2-design.md` (Ardot page “Lyra · Redesign v2”). Kept as historical reference.

# Lyra UI shell — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the accepted cool-gray Ardot design into the real Vite/React app (auth gate + mail chrome). Design files are reference only.

**Architecture:** Extend existing `frontend/` shell. CSS custom properties for the locked palette. Auth = two-column form | COBE panel. Mail = three-pane with rich list rows, column-header icon toolbar, whisper elevation on selected row. Motion for micro-interactions; COBE for auth globe only.

**Tech Stack:** React 19, Vite, TanStack Router, Zustand, XState, `motion`, `cobe`, Instrument Sans/Serif (CDN or fontsource), existing i18n en/zh.

## Global Constraints

- No Voyah mentions; stamp + serif L brand mark.
- Cool utility gray only — no purple/orange chrome; status colors status-only.
- Auth: two columns (white form | `#F0F0F2` globe). No floating card over globe. No marketing SSO/CTAs.
- Whisper shadow allowlist: selected list row, menus/compose (auth form column has no card shadow).
- `prefers-reduced-motion`: skip Motion enters; static or hidden COBE.
- All user-visible copy via i18n (`en` + `zh`).
- Do not commit secrets; design/Ardot stays out of runtime.

## File map

| File | Responsibility |
|------|----------------|
| `frontend/src/index.css` | Design tokens + layout styles |
| `frontend/src/components/stamp-mark.tsx` | Postage stamp SVG (sawtooth + L) |
| `frontend/src/components/auth-globe.tsx` | COBE canvas wrapper |
| `frontend/src/components/login-form.tsx` | Two-column auth UI |
| `frontend/src/components/mail-list.tsx` | Rich rows (status, unread, whisper) |
| `frontend/src/components/mail-view.tsx` | Column-header icon toolbar |
| `frontend/src/components/mail-layout.tsx` | Splitters (later task) |
| `frontend/src/types/index.ts` | Optional `isReplied` on messages |
| `frontend/package.json` | `motion`, `cobe` deps |

---

## Task 1: Dependencies + tokens

**Files:** `frontend/package.json`, `frontend/src/index.css`, `frontend/index.html` (fonts)

- [x] Install `motion` and `cobe` (+ `@types` if needed).
- [x] Load Instrument Sans + Instrument Serif (link or npm).
- [x] Replace `:root` zinc tokens with cool-gray palette (`#F0F0F2`, `#F9F9FA`, `#1A1B1F`, `#E2E2E5`, whisper shadow vars).
- [x] Smoke: `npm run typecheck` in `frontend/`.

## Task 2: Auth two-column + stamp + COBE

**Files:** `stamp-mark.tsx`, `auth-globe.tsx`, `login-form.tsx`, auth CSS, i18n tagline

- [x] Add `StampMark` SVG component (sawtooth edge + serif L).
- [x] Add `AuthGlobe` using `cobe` (cool gray, slow rotate, destroy on unmount; honor reduced motion).
- [x] Restructure `LoginForm` to two columns for login / bootstrap / TOTP / loading.
- [x] Update i18n tagline to “Mail you host yourself.” / zh equivalent.
- [ ] Manual: `npm run dev` → `/login` shows split layout.

## Task 3: Mail list richness

**Files:** `mail-list.tsx`, types, CSS

- [x] Add 12px status column (replied glyph when `isReplied`; empty slot otherwise).
- [x] Unread: ink dot + semibold sender.
- [x] Selected: white card + `shadow-whisper`.
- [ ] Header band aligned with reader (title + search icon control).

## Task 4: Reader column toolbar

**Files:** `mail-view.tsx`, CSS

- [x] Move actions into 48px column header; icon-only groups (manage | respond).
- [x] Reader surface `#F9F9FA`; avatar + meta header.

## Task 5: Splitters (follow-up)

**Files:** `mail-layout.tsx`

- [x] 5px hit targets between sidebar|list and list|reader (visual only; drag persist later).
- [ ] Persist widths in UI store.

---

**Done when:** Auth matches two-column reference; mail list/reader match cool-gray chrome; typecheck + lint pass.
