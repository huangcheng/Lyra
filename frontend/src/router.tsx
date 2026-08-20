/**
 * TanStack Router setup for Lyra.
 *
 * Uses programmatic route definitions (no file-based routing for v1 shell).
 * Auth-gated: redirects to login when not authenticated.
 */

import {
  createRouter as createTanStackRouter,
  createRootRoute,
  createRoute,
  Outlet,
  redirect,
} from '@tanstack/react-router';
import { MailLayout } from './components/mail-layout';
import { AuthPage } from './components/auth-page';
import { useAuthStore } from './stores/auth';

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
  beforeLoad: () => {
    const { isAuthenticated } = useAuthStore.getState();
    if (!isAuthenticated) {
      throw redirect({ to: '/login' });
    }
  },
  component: MailLayout,
});

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  beforeLoad: () => {
    const { isAuthenticated } = useAuthStore.getState();
    if (isAuthenticated) {
      throw redirect({ to: '/' });
    }
  },
  component: AuthPage,
});

// ── Route tree ─────────────────────────────────────────────────

const routeTree = rootRoute.addChildren([indexRoute, loginRoute]);

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
