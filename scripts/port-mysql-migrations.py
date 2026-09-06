#!/usr/bin/env python3
"""Port backend/migrations/sqlite -> backend/migrations/mysql.

Run from anywhere after adding a new sqlite migration:
    python scripts/port-mysql-migrations.py

Special-cased migrations (hand-written MySQL forms):
- 0009 FTS5 virtual table + triggers  -> one multi-column FULLTEXT ngram index
- 0011 self-referencing data migration -> multi-table DELETE + CONCAT

Everything else is a mechanical transform; review the diff.
"""

import io, os, re, glob

SRC = os.path.join(os.path.dirname(__file__), "..", "backend", "migrations", "sqlite")
DST = os.path.join(os.path.dirname(__file__), "..", "backend", "migrations", "mysql")
os.makedirs(DST, exist_ok=True)

ID_RE = re.compile(r'^\s*(id|user_id|account_id|folder_id|thread_id|message_id|calendar_id|subscription_id|parent_id|contact_id|event_id|job_id|key_id|source_account_id)\s+TEXT\b', re.I)
USERNAME_RE = re.compile(r'^\s*(username|email_address|external_id)\s+TEXT\b', re.I)

def transform(body: str, fname: str) -> str:
    out_lines = []
    indexed_cols = set()
    # collect columns named by CREATE INDEX statements
    for m in re.finditer(r'CREATE (?:UNIQUE )?INDEX\s+(?:IF NOT EXISTS\s+)?\w+\s+ON\s+\w+\s*\(([^)]+)\)', body, re.I):
        for col in m.group(1).split(','):
            indexed_cols.add(col.strip().split()[0].lower())
    # plus PRIMARY KEY (...) / UNIQUE (...) clauses inside CREATE TABLE bodies
    for m in re.finditer(r'(?:PRIMARY KEY|UNIQUE)\s*\(([^)]+)\)', body, re.I):
        for col in m.group(1).split(','):
            name = col.strip().split()[0].lower()
            if name not in ('primary', 'unique', 'key', 'constraint'):
                indexed_cols.add(name)
    # plus FOREIGN KEY ... REFERENCES parent columns (child side is the local col before REFERENCES)
    for m in re.finditer(r'FOREIGN KEY\s*\(([^)]+)\)\s*REFERENCES', body, re.I):
        for col in m.group(1).split(','):
            indexed_cols.add(col.strip().split()[0].lower())
    for line in body.splitlines():
        stripped = line.strip()
        # drop pragmas
        if stripped.upper().startswith('PRAGMA'):
            continue
        # sqlite FTS migration is replaced wholesale (handled by caller)
        if 'fts5' in stripped or 'CREATE VIRTUAL TABLE' in stripped.upper():
            continue
        let = line
        # timestamps default
        let = let.replace("DEFAULT (datetime('now'))",
                          "DEFAULT (DATE_FORMAT(UTC_TIMESTAMP(),'%Y-%m-%d %H:%M:%S'))")
        # id-like columns
        m = ID_RE.match(let)
        if m and 'VARCHAR' not in let:
            let = ID_RE.sub(lambda mm: f"{mm.group(1)} VARCHAR(36)", let, count=1)
        # username/email/external
        m = USERNAME_RE.match(let)
        if m and 'VARCHAR' not in let:
            let = USERNAME_RE.sub(lambda mm: f"{mm.group(1)} VARCHAR(190)", let, count=1)
        # indexed columns -> VARCHAR(255)
        first_word = stripped.split()[0] if stripped.split() else ''
        if first_word.lower() in indexed_cols and re.match(r'^\s*\w+\s+TEXT\b', let):
            let = re.sub(r'^(\s*\w+)\s+TEXT\b', r'\1 VARCHAR(255)', let, count=1)
        # booleans + integers
        if re.search(r'\bTINYINT', let) is None:
            if re.search(r'\b(is_[a-z_]+|totp_enabled|all_day|is_all_day)\s+INTEGER\b', let):
                let = re.sub(r'\bINTEGER\b', 'TINYINT(1)', let, count=1)
            elif re.search(r'\bINTEGER\b', let) and 'PRIMARY KEY' not in let:
                let = re.sub(r'\bINTEGER\b', 'BIGINT', let, count=1)
        # index DDL: strip IF NOT EXISTS (unsupported for CREATE INDEX)
        let = let.replace('CREATE INDEX IF NOT EXISTS', 'CREATE INDEX')
        let = let.replace('CREATE UNIQUE INDEX IF NOT EXISTS', 'CREATE UNIQUE INDEX')
        let = let.replace('DROP INDEX IF EXISTS ', 'DROP INDEX ')
        # ALTER TABLE ADD COLUMN <id> TEXT REFERENCES ... -> VARCHAR(36)
        # (MySQL silently ignores inline REFERENCES on ADD COLUMN; the type
        # matters so later CREATE INDEX on the column is legal.)
        m2 = re.match(r"^(\s*ALTER TABLE\s+\w+\s+ADD COLUMN\s+)(\w+)(\s+)TEXT\b", let, re.I)
        if m2:
            let = m2.group(1) + m2.group(2) + m2.group(3) + 'VARCHAR(36)' + let[m2.end():]
        # MySQL: TEXT columns reject literal defaults - parenthesize
        if re.search(r"\bTEXT\b", let) and "DEFAULT '" in let and "DEFAULT (" not in let:
            let = re.sub(r"DEFAULT ('(?:[^']|'')*')", r"DEFAULT (\1)", let)
        out_lines.append(let)
    return '\n'.join(out_lines)

FTS_UP = """-- Full-text search index (MySQL): one multi-column FULLTEXT index with the
-- ngram parser so CJK (zh) content tokenizes; MySQL maintains it from the
-- base table itself (no triggers, unlike the FTS5 build).

CREATE FULLTEXT INDEX message_fts_all ON message (subject, body_text, from_address) WITH PARSER ngram;
"""

MERGE_0011_UP = """-- Merge leftover bare-UID IMAP rows into their folder-scoped twins
-- (MySQL form: multi-table DELETE for the self-join, CONCAT instead of ||).

DELETE m
FROM message m
JOIN message twin
  ON twin.account_id = m.account_id
 AND twin.folder_id = m.folder_id
 AND twin.external_id = CONCAT(m.folder_id, ':', m.external_id)
WHERE m.external_id IS NOT NULL
  AND m.external_id NOT LIKE '%:%'
  AND m.account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );

UPDATE message
SET external_id = CONCAT(folder_id, ':', external_id)
WHERE external_id IS NOT NULL
  AND external_id NOT LIKE '%:%'
  AND account_id IN (
      SELECT id FROM mail_account
      WHERE COALESCE(receive_protocol, protocol) <> 'jmap'
  );
"""

FTS_DOWN = """DROP INDEX message_fts_subject ON message;
DROP INDEX message_fts_body ON message;
DROP INDEX message_fts_from ON message;
"""

count = 0
for path in sorted(glob.glob(os.path.join(SRC, '*.sql'))):
    fname = os.path.basename(path)
    body = io.open(path, encoding='utf-8').read()
    if fname.startswith('0009_message_fts.'):
        out = FTS_UP if '.up.' in fname else FTS_DOWN
    elif fname.startswith('0011_merge_legacy_bare_uid_messages.'):
        out = MERGE_0011_UP if '.up.' in fname else 'DELETE FROM message WHERE 1=0;'
    else:
        out = transform(body, fname)
    io.open(os.path.join(DST, fname), 'w', encoding='utf-8', newline='\n').write(out + ('\n' if not out.endswith('\n') else ''))
    count += 1
print('ported', count, 'files ->', DST)
