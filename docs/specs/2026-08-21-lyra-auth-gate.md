# Lyra — Auth Gate

**Date:** 2026-08-21  
**Status:** Accepted (design direction)  
**Audience:** Design + frontend + auth  
**Product context:** Self-hosted, single-user v1 — not SaaS. No marketing landing page.  
**Visual system:** `docs/specs/2026-08-21-lyra-ui-design.md` (monochrome Geist-like; stamp + serif L).

---

## Goal

Unauthenticated visitors must never see the mail app shell. Entry is an **auth gate** only: setup or login, then the app.

## Non-goals

- Marketing / product landing page
- Pricing, signup funnels, public docs inside the app origin
- Mail-account onboarding (IMAP/JMAP) — that stays **after** Lyra login
- Multi-user, SSO, GitHub, passkeys (later; see product spec)

---

## Routing

| Condition | Destination |
|-----------|-------------|
| No Lyra user exists | **Setup** (create username + password) |
| User exists, no valid session | **Login** (password; TOTP step if enabled) |
| Valid session | Mail app |

- Unauthenticated hits to any protected route redirect to the gate (preserve return path only after successful auth if useful; otherwise `/`).
- Authenticated hits to setup/login redirect into the app.
- Do not render inbox chrome, folders, or Compose behind a modal.

---

## Gate UI

- Full-viewport, single job: authenticate or create the Lyra user.
- Brand: postage stamp + serif **L**, product name **Lyra**, short supporting line optional.
- Theme: monochrome light (dark variant optional for spot-check); accent = near-black CTA / Linear white pills (see UI design language).
- Controls: username, password, primary CTA; TOTP as a second step when required.
- Strong password policy on setup (and change-password later); match product security basics.
- No secondary marketing blocks, feature lists, or “create account” SaaS copy when a user already exists.

### Atmosphere (Login richness)

**Superseded:** see `docs/superpowers/specs/2026-08-21-lyra-auth-editorial-design.md`.

**Layout:** Single editorial stack on cool gray panel — stamp, wordmark, form, tagline. No two-column globe panel. Favicon = stamp mark.

---

## Flows (XState-aligned)

- `authMachine`: idle → authenticating → authenticated | error (optional TOTP branch).
- First-run setup is a distinct path that creates the single user, then enters authenticated.
- Session expiry mid-app: return to login without flashing mail UI with stale data.

---

## Acceptance

- [ ] No session → mail routes never paint the app shell
- [ ] First install → setup; thereafter → login
- [ ] Optional TOTP works as a second step
- [ ] Gate uses stamp + serif L + monochrome
- [ ] No landing / marketing page in v1
