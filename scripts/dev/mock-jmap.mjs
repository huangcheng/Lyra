#!/usr/bin/env node
/**
 * Minimal local JMAP server for testing Lyra's account connection flow.
 *
 *   node scripts/dev/mock-jmap.mjs            # listens on 127.0.0.1:9200
 *   PORT=9300 node scripts/dev/mock-jmap.mjs
 *
 * Auth: HTTP Basic; the password must be `app-pass-1` (username is ignored —
 * use any email like dev@lyra.test). Wrong password reproduces the exact
 * HTTP 401 the real providers send back.
 *
 * Endpoints:
 *   GET  /.well-known/jmap   → session resource (RFC 8620 §2)
 *   POST /jmap               → Core echo, Mailbox/get, Email/get stubs
 *   *                        → 404
 *
 * No TLS: Lyra's netsec rules allow plain http for loopback hosts.
 */
import http from 'node:http';

const PORT = Number(process.env.PORT ?? 9200);
const HOST = '127.0.0.1';
const PASSWORD = 'app-pass-1';
const ORIGIN = `http://${HOST}:${PORT}`;
const ACCOUNT_ID = 'mock-mail-account';

const session = () => ({
  capabilities: {
    'urn:ietf:params:jmap:core': {
      maxSizeRequest: 10_485_760,
      maxConcurrentRequests: 8,
      maxCallsInRequest: 32,
      maxObjectsInGet: 1024,
      maxObjectsInSet: 1024,
      maxConcurrentUploads: 4,
      maxSizeUpload: 50_000_000,
    },
    'urn:ietf:params:jmap:mail': { maxMailboxesPerEmail: 1024 },
    'urn:ietf:params:jmap:submission': { maxDelayedSend: 0 },
  },
  accounts: {
    [ACCOUNT_ID]: {
      name: 'Mock JMAP Account',
      isPersonal: true,
      isReadOnly: false,
      accountCapabilities: {},
      mailAddresses: [{ type: 'inbox', email: 'dev@lyra.test' }],
    },
  },
  primaryAccounts: {
    'urn:ietf:params:jmap:mail': ACCOUNT_ID,
    'urn:ietf:params:jmap:submission': ACCOUNT_ID,
  },
  username: 'dev@lyra.test',
  apiUrl: `${ORIGIN}/jmap`,
  downloadUrl: `${ORIGIN}/download/{accountId}/{blobId}/{name}`,
  uploadUrl: `${ORIGIN}/upload/{accountId}/`,
  eventSourceUrl: `${ORIGIN}/events?types=*&closeafter=no&ping=30`,
  state: `mock-state-${Date.now()}`,
});

function authorized(req) {
  const header = req.headers.authorization ?? '';
  const [scheme, encoded] = header.split(' ');
  if (scheme !== 'Basic' || !encoded) return false;
  const decoded = Buffer.from(encoded, 'base64').toString('utf8');
  const idx = decoded.indexOf(':');
  if (idx < 0) return false;
  return decoded.slice(idx + 1) === PASSWORD;
}

function problem(res, status, title) {
  res.writeHead(status, { 'Content-Type': 'application/problem+json' });
  res.end(JSON.stringify({ status, title, type: 'about:blank' }));
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, ORIGIN);

  if (req.method === 'GET' && url.pathname === '/.well-known/jmap') {
    if (!authorized(req)) return problem(res, 401, 'Invalid credentials');
    res.writeHead(200, { 'Content-Type': 'application/json' });
    return res.end(JSON.stringify(session()));
  }

  if (req.method === 'POST' && url.pathname === '/jmap') {
    if (!authorized(req)) return problem(res, 401, 'Invalid credentials');
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      let calls = [];
      try {
        calls = JSON.parse(body).methodCalls ?? [];
      } catch {
        return problem(res, 400, 'Malformed request');
      }
      const out = calls.map(([name, args, cid]) => {
        if (name === 'Core/echo') return [name, args, cid];
        if (name === 'Mailbox/get')
          return [name, { accountId: args.accountId, state: 'm1', list: [], notFound: [] }, cid];
        if (name === 'Email/query')
          return [name, { accountId: args.accountId, queryState: 'e1', ids: [] }, cid];
        if (name === 'Email/get')
          return [name, { accountId: args.accountId, state: 'e1', list: [], notFound: [] }, cid];
        return ['error', { type: 'unknownMethod' }, cid];
      });
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ methodResponses: out, sessionState: 'mock' }));
    });
    return;
  }

  problem(res, 404, 'Not found');
});

server.listen(PORT, HOST, () => {
  console.log(`mock-jmap listening on ${ORIGIN}`);
  console.log(`credentials: any-username / ${PASSWORD}   (wrong password → 401)`);
});
