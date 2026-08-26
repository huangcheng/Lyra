# Product

## Register

product

## Users

Self-hosters and privacy-conscious individuals who run Lyra on their own hardware (Docker Compose). Single-user today, multi-user-ready data shape. They connect real mail accounts (Fastmail, Outlook, any IMAP/JMAP provider) and live in the three-pane mail view daily. Bilingual: English and Chinese.

## Product Purpose

Lyra is a self-hosted mail **client** (not a mail server). Prefer JMAP, fall back to IMAP; send via SMTP. Everything the UI does goes through a client-agnostic `/api/v1`. Success looks like: a calm, fast, trustworthy daily mail home that you host yourself, with sync that is idempotent, resumable, and never leaks secrets.

## Brand Personality

Quiet, precise, crafted. No hype, no decoration without purpose. The brand mark is a postage stamp: a solid ink square with a serif "L". Tagline: "Mail you host yourself."

## Anti-references

- Stock shadcn/ui mail example look (the v1 UI, judged too generic)
- Indigo/purple-blue SaaS palettes, gradient text, glassmorphism, glow
- Solid black or brand-colored CTA buttons
- Hero-metric dashboard templates and identical icon-card grids
- Nested cards, drop shadows, colored side-stripe borders

## Design Principles

- **Color means status.** Amber = unread/today, green = healthy/on, muted red = destructive text. Everything else is cool gray.
- **Restraint is the brand.** Hairlines instead of shadows; white pill buttons with hairline borders instead of solid fills.
- **Mail is home.** Dashboard and Settings are standalone destinations sharing one slim-nav shell; neither carries the mail sidebar.
- **Both themes, both languages.** Every screen ships in light + dark and en + zh; status colors stay constant across themes.
- **Hide the protocol.** IMAP/JMAP/OAuth complexity surfaces only where the user asks for it (Settings), never in the reading flow.

## Accessibility & Inclusion

WCAG AA contrast minimum. Visible focus indicators, logical tab order, 44px touch targets, `prefers-reduced-motion` respected. Chinese renders via system font fallbacks with equal care to metrics and truncation.
