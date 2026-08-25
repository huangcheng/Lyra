# Lyra OpenGPG Support — Phase Spec

**Date:** 2026-08-23
**Status:** Planned (separate track from the [remote-image proxy spec](./2026-08-23-lyra-remote-image-proxy-spec.md))
**Scope:** End-to-end OpenGPG for mail handled by Lyra: key management, decrypt + verify on read, sign + encrypt on send.

**Naming:** Lyra calls this feature **OpenGPG** (open, GnuPG-family crypto) in product copy, API paths, schema, and UI — not "PGP" (commercial trademark). On the wire we implement the **OpenPGP** format (RFC 4880 / RFC 3156) and interoperate with **GnuPG** and other OpenPGP clients. RFC/MIME literals (`application/pgp-encrypted`, `BEGIN PGP MESSAGE`, `sequoia-openpgp` crate) stay as wire-format names.

---

## Why

Lyra stores mail on the self-hosted box and transit between client and backend is already protected (sessions, TLS, encrypted credentials at rest). OpenGPG closes the remaining gap: content encrypted end-to-end between correspondents, so neither upstream providers nor anyone reaching the stored mail files can read protected messages. It also enables signing, giving recipients verifiable authenticity.

## Goals

- Manage OpenGPG keys inside Lyra (generate, import, export, delete, list) — one keyring per user.
- Decrypt incoming OpenGPG messages and verify signatures, transparently in the read path.
- Sign and encrypt are **independent operations**: sign-only, encrypt-only, both, or neither — per message, user-controlled. Defaults: sign if a secret key is unlocked; encrypt only when every recipient has a known public key.
- Support **OpenPGP/MIME** (RFC 3156, `multipart/encrypted` / `multipart/signed`) as the primary wire format; inline armored messages best-effort on receive.
- Secret keys are **always stored passphrase-encrypted** (native OpenPGP armored protection) — an unlocked secret key exists only in server memory for the duration of an authenticated session, never on disk.
- Everything behind the existing `/api/v1` auth gate; web is a peer client, so desktop/mobile later get OpenGPG for free.

## Non-goals (this phase)

- No S/MIME. No autocrypt full-consensus protocol (we may borrow its "attach my public key" behavior).
- No web-of-trust scoring; trust is a simple user-controlled flag.
- No re-encryption of already-stored mail; OpenGPG applies per-message at read/write time.
- No per-recipient fine-grained policies beyond "key exists / trusted".

## Key architectural decision: server-side crypto

Decryption/encryption happens **in the backend**, not the browser:

- Lyra's model is server-side sync + index + search; client-side-only decryption would break search and every non-web client.
- The instance is single-user and self-hosted, so "server" and "owner" are the same trust domain.
- Secret keys are stored encrypted under the user DEK (derived from `LYRA_MASTER_KEY`), same envelope as mail-account credentials (`docs/specs/2026-08-20-lyra-data-model-spec.md` §3).

Consequence (accepted): the backend sees plaintext of decrypted mail. That matches how all other mail is already stored/indexed by Lyra.

### Library choice

| Option | Pros | Cons |
|--------|------|------|
| **sequoia-pgp** (recommended) | Most complete Rust OpenPGP implementation; active; signature policy engine; `sequoia-openpgp` handles MIME parsing cleanly | LGPL-2.0+; nettle backend pulls a C dependency (Docker image must install nettle dev libs, or use the `crypto-net`/OpenSSL backend) |
| rPGP (`pgp` crate) | MIT/Apache, pure Rust, small | Less complete policy handling, weaker ergonomics for certification/revocation |

Decision: **rPGP** (`pgp` crate) for P1 — pure Rust, MIT/Apache, no nettle/C toolchain in Docker. Sequoia remains a future option if we need its policy engine; document any switch here.

### Key protection: passphrase-locked keys, user-chosen unlock caching (decided)

Storing an unwrapped secret key at rest is insecure and rejected. Instead:

- Secret keys are persisted **as-is in their passphrase-protected armored form**. The passphrase is never stored, never derived from `LYRA_MASTER_KEY`.
- Import keeps the key's existing passphrase protection. Keygen requires the user to choose a passphrase; the generated secret key is written to the DB already locked with it.
- To use a secret key (decrypt incoming, sign outgoing), the user **unlocks** it; how long the unlock is remembered is **the user's choice, git/gpg-agent-style**:
  - `POST /api/v1/opengpg/unlock { key_id, passphrase, cache: "once" | "timed" | "session" }` — backend attempts to unlock; on success the unlocked key lives in a per-session, in-memory keyring (in the auth session state, not a global map keyed by token).
  - `once` — passphrase prompt every time the key is needed (nothing cached beyond the current request batch).
  - `timed` — cached for a TTL (default 10 min, gpg-agent's `default-cache-ttl` analog; user-configurable 1–120 min).
  - `session` — cached until logout or explicit lock.
  - `POST /api/v1/opengpg/lock` clears immediately (also cleared on logout). Idle-timeout relock applies in all modes.
  - The chosen mode is persisted as a preference (`/api/v1/settings/opengpg` → `{ passphrase_cache: { mode, ttl_minutes } }`) and preselected in the unlock dialog; the unlock prompt shows a "remember passphrase" choice exactly like git credential helpers, so it can be overridden per unlock.
- Read paths needing a locked key return `opengpg.error = "locked"`; the UI shows an inline passphrase prompt (XState machine) with the remember-choice control and retries the request after unlock.
- Signing on send: if no key is unlocked, compose shows "signing disabled — unlock your key" rather than silently dropping the signature.

## Schema (dual-DB, migration `0008_opengpg_keys`)

> Note: `0007` is already used for `folder.role_override` (CHE-128). OpenGPG keys ship as **`0008_opengpg_keys`**.

```sql
CREATE TABLE opengpg_key (
  id            TEXT PRIMARY KEY,            -- uuid
  user_id       TEXT NOT NULL REFERENCES lyra_user(id) ON DELETE CASCADE,
  fingerprint   TEXT NOT NULL,               -- uppercase hex, v4
  primary_email TEXT NOT NULL,               -- primary uid, lowercased
  emails        TEXT NOT NULL DEFAULT '[]',  -- JSON array of uid emails
  is_secret     BOOLEAN NOT NULL,            -- secret keypair vs public-only
  is_primary    BOOLEAN NOT NULL DEFAULT 0,  -- the user's own signing key
  revoked       BOOLEAN NOT NULL DEFAULT 0,
  key_data      TEXT NOT NULL,               -- armored; secret keys stay passphrase-locked (never unwrapped at rest)
  created_at    TEXT NOT NULL,               -- UTC
  updated_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_opengpg_key_user_fp ON opengpg_key (user_id, fingerprint);
CREATE INDEX idx_opengpg_key_user_email ON opengpg_key (user_id, primary_email);
```

(Adapt types for Postgres per the dual-DB conventions: `jsonb`, `timestamptz`, `uuid` handled in the query layer.)

## API surface (`/api/v1/opengpg`)

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/opengpg/keys` | List keys (public fields only; secret keys marked, never return key material) |
| POST | `/opengpg/keys` | Import armored key (auto-detect public/secret); reject multi-secret bundles > 1 |
| POST | `/opengpg/keys/generate` | `{ email, name, passphrase }` → new keypair (see algorithms below), locked with the passphrase, mark `is_primary` |
| POST | `/opengpg/unlock` | `{ key_id, passphrase, cache }` → unlock secret key; `cache` = `once` / `timed` / `session` (see key protection) |
| POST | `/opengpg/lock` | Clear unlocked keys for this session |
| GET | `/opengpg/keys/{id}` | Details incl. uids, expiry, certifications |
| DELETE | `/opengpg/keys/{id}` | Delete (refuse deleting `is_primary` unless another is promoted) |
| GET | `/opengpg/keys/{id}/export` | Armored export; secret export requires re-auth + explicit `include_secret=true` |
| PATCH | `/opengpg/keys/{id}` | Set `is_primary`, trust flag |

Message responses grow an `opengpg` block (all endpoints that return a message):

```json
"opengpg": {
  "encrypted": true, "decrypted": true,
  "signatures": [{ "fingerprint": "…", "email": "…", "valid": true, "time": "…" }],
  "error": null            // e.g. "no matching secret key"
}
```

Send path: compose request gains `opengpg: { encrypt: bool, sign: bool, attach_public_key: bool }` — `encrypt` and `sign` are fully independent (defaults: sign if secret key is **unlocked**; encrypt if all recipients have keys). All four combinations are valid wire states.

### Algorithm choice (decided): RSA-4096

Keygen default is **RSA-4096** (signing key + RSA-4096 encryption subkey, AES-256 cipher preference) — the strongest widely-interoperable option today. Ed25519/cv25519 remains available as an explicit "modern/compact" choice in the generate dialog; ed448/x448 is recorded as a future option once MUA support is common. All algorithms are always accepted on import/decrypt regardless.

## Phases

### P1 — Key store & management (foundation)
- Migration `0008_opengpg_keys` (sqlite + postgres + up/down). **done** (CHE-63)
- `backend/src/opengpg/` module: `mod.rs`, `keys.rs` (cert parsing, fingerprinting), `store.rs` (DB seam), `session.rs` (unlock ring). **done** (CHE-63/64).
- `/api/v1/opengpg/keys` CRUD + generate + export (re-auth for secrets). **done** (CHE-61); `unlock`/`lock` + settings preference. **done** (CHE-64).
- Frontend: Settings → "Encryption" page: list/import/export/primary selection; unlock prompt (XState) with idle-relock indicator. **done** (CHE-62).
- Tests at the seam: import → list → export roundtrip; wrong passphrase on unlock rejected; unlocked material absent from DB/serialized session state.

### P2 — Decrypt & verify on read
- Detect in `get_message`: `Content-Type: application/pgp-encrypted` parts and inline `-----BEGIN PGP MESSAGE-----` in text bodies. **done** (CHE-65; also cleartext signed + OpenPGP/MIME attachment candidates)
- Decrypt OpenPGP/MIME at serve time; replace `body_text`/`body_html` (HTML still passes `persist_body_html` sanitization) and expose inner attachments through the existing attachment mechanism. **done** for body replace (CHE-65); inner attachment re-expose deferred if needed
- Signature verification results into the `opengpg` response block. **done** (CHE-65)
- UI: lock/shield badge on message header, signature status line, `locked` state triggers the inline passphrase prompt. **done** (CHE-66)
- Do **not** persist decrypted content in v-scope; decrypt per request (measure latency; add an in-memory LRU keyed by message id only if needed). **done** (CHE-65)

### P3 — Sign & encrypt on send
- Extend send pipeline (`sync/send.rs`): build OpenPGP/MIME (RFC 3156) multipart; inline fallback off by default. **done** (CHE-67)
- Four explicit combinations: **sign-only** (`multipart/signed`), **encrypt-only** (`multipart/encrypted`, unsigned), **sign+encrypt**, **plain** — nothing is implicit beyond the documented defaults. **done** (CHE-67)
- Recipient key resolution: exact-match on `emails` in the keyring; ambiguous → ask in compose UI. **done** (CHE-67/68; `GET /api/v1/opengpg/recipient-keys`)
- Optional "attach my public key" adds an `application/pgp-keys` part. **done** (CHE-67)
- UI: independent compose toggles (encrypt / sign), per-recipient key indicators, error when encrypting without a key; signing with a locked key offers the unlock prompt inline. **done** (CHE-68)

### P4 — Discovery & polish (optional, after P1–P3 are stable)
- Web Key Directory (WKD) lookup for unknown recipients (respects `netsec` SSRF guards).
- Autocrypt-style: parse `Autocrypt:` headers from incoming mail to opportunistically import peer public keys (off by default).
- Key expiry/revocation refresh, key-change warnings ("this contact's key changed").
- Search consideration: optionally index decrypted plaintext of encrypted mail at ingest-time decrypt so search covers it — decide after P2 usage feedback (privacy tradeoff documented).

## Security rules

- Unlocked secret keys exist only in per-session memory; zeroize on drop, on lock, on logout, and on idle timeout.
- No plaintext, key material, passphrases, or fingerprints in logs or tracing spans.
- Secret export requires fresh session confirmation; response is one-shot; exporting a secret key requires it unlocked (passphrase re-entry serves as confirmation).
- Decryption failures return typed errors (`OpenGpgError`), never panic paths into the sync loop.
- Malformed armored input must never crash ingestion: decrypt is read-path only; ingest stores the encrypted MIME untouched.
- Unlock attempts are rate-limited (per session) to blunt passphrase guessing.

## Resolved decisions

1. **Secret-key storage:** passphrase prompt; keys stay passphrase-locked at rest (never unwrapped to disk). ✔
2. **Keygen algorithm:** RSA-4096 default (strongest widely-compatible); ed25519/cv25519 as explicit option. ✔
3. **Sign vs encrypt:** independent per-message toggles; sign-only is a first-class scenario. ✔
4. **Passphrase caching:** user chooses `once` / `timed` (TTL configurable, default 10 min) / `session`, git/gpg-agent-style, with a persisted preference. ✔

## Open questions

1. Do we index decrypted text for search (P4) or leave encrypted mail unsearchable by content?

## Verification

- Unit/integration tests in `backend/src/opengpg/` at the module seam (`cargo test --bin lyra_backend`).
- Interop check list: **GnuPG** (CLI), Thunderbird/Enigmail, Proton, Delta Chat — decrypt our send, we decrypt theirs.
- `make check` green; no new warnings.
