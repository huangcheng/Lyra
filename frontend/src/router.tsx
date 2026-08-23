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
import { SettingsPage } from './components/settings-page';
import { ContactsPage } from './components/contacts-page';
import { CalendarPage } from './components/calendar-page';
import { useAuthStore } from './stores/auth';
import { useSyncEventSource } from './lib/use-sync-event-source';

// ── Routes ─────────────────────────────────────────────────────

const rootRoute = createRootRoute({
  component: RootLayout,
});

function RootLayout() {
  useSyncEventSource();
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

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  beforeLoad: () => {
    const { isAuthenticated } = useAuthStore.getState();
    if (!isAuthenticated) {
      throw redirect({ to: '/login' });
    }
  },
  component: SettingsPage,
});

const contactsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/contacts',
  beforeLoad: () => {
    const { isAuthenticated } = useAuthStore.getState();
    if (!isAuthenticated) {
      throw redirect({ to: '/login' });
    }
  },
  component: ContactsPage,
});

const calendarRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/calendar',
  beforeLoad: () => {
    const { isAuthenticated } = useAuthStore.getState();
    if (!isAuthenticated) {
      throw redirect({ to: '/login' });
    }
  },
  component: CalendarPage,
});

// ── Route tree ─────────────────────────────────────────────────

const routeTree = rootRoute.addChildren([
  indexRoute,
  loginRoute,
  settingsRoute,
  contactsRoute,
  calendarRoute,
]);

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
