import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { createRouter } from './router';
import { startViewStatePersistence } from './lib/persist-view-state';
import { restoreSession } from './lib/session';
import { initTheme } from './lib/theme';
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
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
});
