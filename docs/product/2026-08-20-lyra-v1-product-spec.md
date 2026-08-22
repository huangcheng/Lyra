# Lyra v1 — Product Spec

**Date:** 2026-08-20  
**Status:** Draft  
**Audience:** Self-hosters who want a private, always-on personal email client

---

## Version summary

v1 is complete when all of the following are done:

1. **Self-hosted app** — single-user login (username/password + optional TOTP); multi-user reserved for later  
2. **Multi-account mail** — add and manage multiple mail accounts  
3. **Protocols** — JMAP when available; IMAP fallback; SMTP for send  
4. **Sync engine** — download, store, and index mail on the server  
5. **Core mail UX** — folders, threaded conversations, read/compose, search, attachments, flags/priority  
6. **Auto server config** — Thunderbird / Apple Mail–style configuration probe  
7. **CardDAV + CalDAV** — contacts and calendar (no Google/Outlook-specific APIs)  
8. **UI** — ship the [shadcn mail](https://v3.shadcn.com/examples/mail) example as the product UI; extend for Lyra needs  
9. **i18n** — English and Chinese (zh) for all user-facing strings  
10. **Deploy** — Docker Compose (recommended) and install script  
11. **Data layer** — carefully designed schema supporting SQLite and PostgreSQL  
12. **Security basics** — HTTPS, encrypted credentials at rest, strong password policy for Lyra login  

**Not required to complete v1:** team collaboration, SaaS hosting, multi-user logins, spam ML beyond provider flags, AI assist / BYOK, native desktop/mobile apps, GitHub/SSO/passkeys, Google/Outlook calendar APIs.

---

## Vision & positioning

Lyra is a self-hosted, always-on email client: Thunderbird / Apple Mail in spirit, but the app and sync engine run on *your* server and you use it from a browser.

| | |
|---|---|
| **For** | Self-hosters who want a private personal mail client they control |
| **Not for (v1)** | Teams, collaboration suites, hosted SaaS |
| **Later** | Multi-user logins, SaaS, GitHub/SSO/passkeys, provider-specific calendar APIs |

**Core job:** Connect multiple mail accounts (JMAP preferred, IMAP fallback), sync and index mail locally, read and compose from anywhere; plus CardDAV contacts and CalDAV calendar.

Lyra is a **client**, not a mail server: it does not provide SMTP/IMAP service of its own.

---

## Goals & non-goals

### Goals (v1)

- Give self-hosters a private, always-on mail client they control  
- Feel familiar if you know Thunderbird or Apple Mail  
- Sync mail to the server so search and multi-device use stay fast  
- Prefer JMAP; fall back to IMAP; auto-detect server settings  
- Include CardDAV contacts and CalDAV calendar  
- One-box deploy via Docker Compose (recommended) or install script  
- Schema that runs cleanly on both SQLite and PostgreSQL  
- Ship a product-ready UI from day one (shadcn mail), with English and Chinese  

### Non-goals (v1)

- Not a mail server (no first-party SMTP/IMAP server)  
- Not team/collaboration (shared drafts, shared inboxes)  
- Not multi-user logins (single user now; design so multi-user can land later)  
- Not SaaS / multi-tenant hosting  
- Not Google/Outlook-specific calendar APIs  
- Not native desktop or mobile apps (responsive web is enough; see multi-client roadmap for later)  
- Not SSO / GitHub / passkeys yet (optional TOTP only)  

---

## Tech & architecture

| Area | Decision |
|------|----------|
| Frontend | React (current), TanStack Router |
| UI | Use [shadcn mail](https://v3.shadcn.com/examples/mail) directly as the v1 product UI; extend for accounts, sync, settings, auth |
| i18n | Required; ship **English** and **Chinese (zh)** |
| Backend | Rust + **Axum** |
| Mail model | Sync engine on the server (store + index locally) |
| Protocols | JMAP preferred; IMAP fallback; SMTP for send |
| PIM | CardDAV + CalDAV |
| Auth (Lyra) | Username/password + optional TOTP; SSO / GitHub / passkeys later |
| Users | Single-user instance; schema/API shaped so multi-user can land later |
| Database | Dual support: SQLite **and** PostgreSQL via ORM; schema designed carefully for both |
| UI state | **Zustand** — lightweight app/UI state |
| Flow control | **XState** — explicit state machines for multi-step flows |
| Async / recovery | **RxJS** — complex streams, orchestration, and error recovery |
| Deploy | Docker Compose (recommended) + install script |
| Transport | HTTPS; secrets/credentials encrypted at rest |

**Client-state roles (do not overlap):**

- **Zustand** — normalized UI and domain data the views read/write  
- **XState** — stepwise flows (onboarding, account setup, auth, sync lifecycle)  
- **RxJS** — long-lived async pipelines (sync workers, retries, backpressure, error recovery)

Detailed sync algorithms, table schemas, and API shapes belong under `docs/specs/`, not this document.

---

## User experience

- Desktop-first three-pane mail layout from shadcn mail (folders / list / reading pane)  
- Default view is a **unified inbox** across accounts; the account switcher still opens a single account  
- Login uses the [shadcn login-01](https://ui.shadcn.com/blocks/login) card (username/password; same card for bootstrap and TOTP)  
- Responsive enough for phone and tablet; no native app in v1  
- Familiar mail actions: compose, reply/forward, flags, search, attachments, threads  
- Account switcher for multiple accounts (plus unified inbox as the default)  
- Settings: theme, accounts, sync, security (2FA), language (en / zh)  
- All user-facing copy goes through i18n; no hardcoded English-only UI strings  

---

## Security

- HTTPS in production  
- Lyra login: strong password policy + optional TOTP  
- Mail-account credentials encrypted at rest  
- Least privilege for stored secrets; no plaintext passwords in logs  
- Keep dependencies updated as an ongoing process  

---

## Deployment

- **Recommended:** Docker Compose one-command bring-up  
- **Also supported:** install script (binary + service unit where applicable)  
- Document SQLite for simplest single-box installs and PostgreSQL for operators who prefer it  

---

## Success criteria

v1 succeeds when a self-hoster can:

1. Bring up Lyra via Docker Compose or the install script  
2. Log in (and optionally enable TOTP)  
3. Add at least two mail accounts (JMAP and/or IMAP), with auto-config helping where possible  
4. Sync, read, compose, search, and manage attachments reliably  
5. Use CardDAV contacts and CalDAV calendar  
6. Switch UI language between English and Chinese  
7. Restart the box and keep data; run the same schema on SQLite or PostgreSQL  

---

## Out of scope → later

| Item | Notes |
|------|--------|
| Multi-user logins | Design schema/API for it; implement after v1 |
| SaaS / multi-tenant | Possible future product surface |
| SSO / GitHub / passkeys | After password + TOTP |
| Google / Outlook calendar APIs | CardDAV/CalDAV only in v1 |
| Native desktop / mobile apps | Responsive web first in v1; far-horizon order in `docs/product/2026-08-20-lyra-multi-client-roadmap.md` (API → desktop → mobile) |
| AI assist (BYOK) | Post-v1; see `docs/product/2026-08-21-lyra-ai-assist-roadmap.md` (draft → categorize → spam → calendar-from-email) |
| Collaboration | Shared drafts, shared mailboxes — not a Lyra v1 goal |
| Advanced spam / ML | Rely on provider flags in v1; optional BYOK spam assist later (same AI roadmap) |

---

## Related docs

- Agent guidance (living): `AGENTS.md`  
- Engineering standards: `docs/specs/2026-08-20-lyra-engineering-standards.md`  
- Multi-client roadmap (far horizon): `docs/product/2026-08-20-lyra-multi-client-roadmap.md`  
- AI assist roadmap (post-v1 BYOK): `docs/product/2026-08-21-lyra-ai-assist-roadmap.md`  
- Further technical specs: `docs/specs/`  
