/**
 * TanStack Router setup for Lyra.
 *
 * Uses programmatic route definitions (no file-based routing for v1 shell).
 */

import {
  createRouter as createTanStackRouter,
  createRootRoute,
  createRoute,
  Outlet,
} from '@tanstack/react-router';
import { MailLayout } from './components/mail-layout';

// ── Routes ─────────────────────────────────────────────────────

const rootRoute = createRootRoute({
  component: RootLayout,
});

function RootLayout() {
  return (
    <div className="app-root">
      <Outlet />
    </div>
  );
}

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: MailLayout,
});

// ── Route tree ─────────────────────────────────────────────────

const routeTree = rootRoute.addChildren([indexRoute]);

// ── Router factory ─────────────────────────────────────────────

export function createRouter() {
  return createTanStackRouter({
    routeTree,
    defaultPreload: 'intent',
  });
}

declare module '@tanstack/react-router' {
  interface Register {
    router: ReturnType<typeof createRouter>;
  }
}
