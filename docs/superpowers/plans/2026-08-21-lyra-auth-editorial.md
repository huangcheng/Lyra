# Auth Editorial Stack — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace two-column + COBE auth with single editorial stack; stamp favicon.

**Tech:** React, Motion, existing cool-gray CSS tokens.

### Task 1: Spec + shell CSS/JSX

**Files:** `login-form.tsx`, `index.css`, `docs/specs/2026-08-21-lyra-auth-gate.md` (pointer)

- Drop `AuthGlobe`; single `.auth-page` → `.auth-stack`
- Grain/vignette background; ~380px column
- Tagline under stack

### Task 2: Favicon

**Files:** `frontend/public/favicon.svg`, `index.html`

- Export stamp SVG; link as icon

### Task 3: Verify

- `npm run typecheck`; visual check at `/login`
