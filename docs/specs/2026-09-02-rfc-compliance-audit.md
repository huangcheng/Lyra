# RFC Compliance Audit — 2026-09-02

Full-project review against the standards table in sync spec §13.1.1.
Audited in worktree `.worktrees/rfc-audit` (branch `rfc-compliance-audit`);
violations fixed in the same branch. Method: per-area code inspection at
the wire seams (adapters, auth, crypto, verification, PIM), each finding
verified at its file:line before inclusion.

## Verdict per area

| Area | RFCs | Verdict | Notes |
|---|---|---|---|
| Message format / MIME | 5322, 2045–2049, 2047, 2231, 6532 | **pass** | mail-parser at ingest; `decode_mime_header` handles B/Q encoded-words; filenames via mail-parser (2231) |
| Threading | 5322 §3.6.4 | **was VIOLATING → fixed** | Grouping was subject-only; verification codes bundled into fake threads. Now: union-find over `In-Reply-To`/`References`, identical-Message-ID copies union, `Re:`/`回复:`-prefix fallback only. Forwards never thread. `frontend/src/lib/conversation.ts` |
| Threading payload | 5322 | **fixed** | `in_reply_to` + `references_headers` (capped 2048 chars) now in list/detail responses |
| IMAP base | 3501, 9051 | pass | literals, parenthesized att lists, capability gates per §13.2 |
| IMAP IDLE | 2177 | pass | renews at 25 min (< 29 min bound); init/done under command timeout |
| IMAP MOVE / delete | 6851, 4315 | **was UNSAFE → fixed** | No-UIDPLUS fallback issued mailbox-wide `EXPUNGE`, destroying *other* sessions' `\Deleted` mail. Now `UID SEARCH DELETED` first; wide EXPUNGE only when the target is the sole flagged message, else the flag stays (local row already moved). `backend/src/imap.rs::expunge_uid_if_safe` |
| CONDSTORE / SPECIAL-USE / UTF-7 | 7162, 6154, 3501 §5.1.3 | pass | per §13.2; wire names preserved in `external_id` (UTF8=ACCEPT not required) |
| JMAP | 8620, 8621 | pass | discovery, EventSource push (§7.3 supervisor), blob download |
| SMTP submission | 5321, 6409, 8314, 4954, 3207, 6531 | pass | 465 implicit TLS / 587 STARTTLS; SMTPUTF8 capability-checked with explicit failure |
| OAuth | 6749, 7636, 7628 | pass | 43-char base64url verifier, S256 challenge; state single-use via kv; XOAUTH2 SASL string exact (`user=…\x01auth=Bearer …\x01\x01`) |
| TOTP | 6238 | pass | SHA1, 6 digits, 30 s step, ±1 window, step-based replay protection |
| DKIM | 6376, 8301 | pass | mail-auth verification; aligned-best selection; temperror backoff |
| DMARC/BIMI gate | 7489 | pass (documented approximation) | org-domain tree walk approximated without PSL (documented in `avatars.rs`); BIMI is a draft, gate is client-side by design |
| OpenPGP | 9580 | pass | per opengpg spec; v4 with 9580 crypto-refresh targets |
| PIM | 5545/5546, 6350 | pass | iCalendar view; vCard PHOTO extraction to blob store |
| Autoconfig | ISPDB, 6186 | pass | ISPDB probe + SRV record discovery (`probe_srv_records`) |

## Known gaps (accepted, not violations)

- **RFC 3676 format=flowed**: plain-text bodies render without reflowing
  `format=flowed` quoting. LOW; HTML bodies dominate real traffic.
- **DMARC org-domain approximation**: no public-suffix list in the tree;
  documented heuristic (last-2/3 labels). Reviewed and accepted in the
  avatar spec.
- **SPF (7208)**: not evaluated client-side. A receiving-client concern
  only if we act as a deliverability reporter; not applicable to a
  reading client beyond the DMARC gate.

## Verification

- Backend 423 tests, frontend 129 tests green on the audit branch.
- New regression tests: same-subject automail never threads; references
  chain transitively; forward prefix does not thread; cross-folder copies
  collapse by Message-ID within a thread.
