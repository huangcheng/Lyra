import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { createRouter } from './router';
import { startViewStatePersistence } from './lib/persist-view-state';
import { restoreSession } from './lib/session';
import { initTheme } from './lib/theme';
import { registerServiceWorker } from './lib/pwa';
import { openMessage, setOpenMessageNavigator } from './lib/notifications';
import '@fontsource-variable/inter';
import '@fontsource-variable/inter-tight';
import '@fontsource/instrument-serif';
import './index.css';

initTheme();
void restoreSession().then(() => {
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
  registerServiceWorker();
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
});
