import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { createRouter } from './router';
import { startViewStatePersistence } from './lib/persist-view-state';
import { restoreSession } from './lib/session';
import { initTheme } from './lib/theme';
import { Sentry, fetchVersionInfo, initSentryFromVersion } from './lib/sentry';
import { registerServiceWorker } from './lib/pwa';
import { openMessage, setOpenMessageNavigator } from './lib/notifications';
import { reconcilePushSubscription } from './lib/push';
import '@fontsource-variable/inter';
import '@fontsource-variable/inter-tight';
import '@fontsource/instrument-serif';
import '@fontsource/instrument-serif/400-italic.css';
import './index.css';

initTheme();
void Promise.all([restoreSession(), fetchVersionInfo()]).then(([, versionInfo]) => {
  // Errors from the very first paint reach Sentry too.
  initSentryFromVersion(versionInfo);
  // Subscribe only after the server state is applied, so the restore itself
  // doesn't echo back as a save.
  startViewStatePersistence();
  const router = createRouter();
  setOpenMessageNavigator(() => router.navigate({ to: '/' }));
  // Notification clicks arrive as service-worker messages.
  navigator.serviceWorker?.addEventListener('message', (ev) => {
    const data = ev.data as { type?: string; messageId?: string } | null;
    if (data?.type === 'lyra:open-message') void openMessage(data.messageId ?? '');
  });
  // Heal the server-side push subscription when this browser has one (covers
  // pushservice endpoint rotation; cheap idempotent PUT).
  void reconcilePushSubscription().catch(() => {});
  registerServiceWorker();
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <Sentry.ErrorBoundary
        fallback={
          <div className="flex h-dvh flex-col items-center justify-center gap-3 bg-background px-6 text-center text-foreground">
            <p className="text-base font-medium">Something went wrong / 页面出错了</p>
            <p className="max-w-sm text-sm text-muted-foreground">
              The error has been reported. Reload to continue. / 错误已上报，刷新页面继续。
            </p>
            <button
              type="button"
              className="rounded-full bg-primary px-4 py-1.5 text-sm text-primary-foreground"
              onClick={() => window.location.reload()}
            >
              Reload / 刷新
            </button>
          </div>
        }
      >
        <RouterProvider router={router} />
      </Sentry.ErrorBoundary>
    </StrictMode>,
  );
});
