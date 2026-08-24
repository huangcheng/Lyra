import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { createRouter } from './router';
import { restoreSession } from './lib/session';
import { initTheme } from './lib/theme';
import '@fontsource-variable/geist';
import './index.css';

initTheme();
void restoreSession().then(() => {
  const router = createRouter();
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
});
