/**
 * Lyra service worker.
 *
 * Responsibilities:
 *  - App-shell caching so the installed PWA opens instantly (and offline).
 *  - Displaying mail notifications on behalf of the page (works from
 *    background tabs; the page's JS keeps the SSE stream alive).
 *  - Focusing + routing to a message on notification click.
 *
 * Deliberately NOT cached:
 *  - `/api/*` — authenticated, mutation-bearing, and SSE (EventSource)
 *    responses must never come from a cache.
 *
 * Update strategy: version-bump `VERSION` (or any shell asset URL); the new
 * worker installs in parallel, old caches are dropped on activate, and the
 * page shows the update after the next reload. `SKIP_WAITING` speeds that
 * up when the UI asks for it.
 */

const VERSION = 'lyra-v1';
const SHELL_CACHE = `${VERSION}-shell`;
const RUNTIME_CACHE = `${VERSION}-runtime`;

const SHELL_ASSETS = [
  '/',
  '/index.html',
  '/manifest.webmanifest',
  '/favicon.svg',
  '/icons/icon-192.png',
  '/icons/icon-512.png',
  '/icons/icon-maskable-512.png',
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(SHELL_CACHE);
      // addAll fails atomically; install what we can so an offline open
      // still works even if one asset 404s.
      await Promise.allSettled(SHELL_ASSETS.map((url) => cache.add(url)));
    })(),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(names.filter((n) => !n.startsWith(VERSION)).map((n) => caches.delete(n)));
      await self.clients.claim();
    })(),
  );
});

self.addEventListener('message', (event) => {
  if (event.data === 'SKIP_WAITING') self.skipWaiting();
});

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;

  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/api/')) return;

  if (req.mode === 'navigate') {
    // Network-first for the SPA shell; cached index.html when offline.
    event.respondWith(
      (async () => {
        try {
          return await fetch(req);
        } catch {
          const cache = await caches.open(SHELL_CACHE);
          return (await cache.match('/index.html')) ?? Response.error();
        }
      })(),
    );
    return;
  }

  if (url.pathname.startsWith('/assets/')) {
    // Vite emits content-hashed, immutable assets — cache-first.
    event.respondWith(
      (async () => {
        const cache = await caches.open(RUNTIME_CACHE);
        const hit = await cache.match(req);
        if (hit) return hit;
        const res = await fetch(req);
        if (res.ok) cache.put(req, res.clone());
        return res;
      })(),
    );
    return;
  }

  // Other same-origin static files (icons, fonts): stale-while-revalidate.
  event.respondWith(
    (async () => {
      const cache = await caches.open(RUNTIME_CACHE);
      const hit = await cache.match(req);
      const refreshing = fetch(req)
        .then((res) => {
          if (res.ok) cache.put(req, res.clone());
          return res;
        })
        .catch(() => undefined);
      return hit ?? refreshing ?? Response.error();
    })(),
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const messageId = event.notification.data?.messageId;
  event.waitUntil(
    (async () => {
      const clientsArr = await self.clients.matchAll({
        type: 'window',
        includeUncontrolled: true,
      });
      let client = clientsArr.find((c) => c.url.includes(self.location.origin));
      if (client) {
        await client.focus();
      } else {
        client = await self.clients.openWindow('/');
      }
      client?.postMessage({ type: 'lyra:open-message', messageId });
    })(),
  );
});
