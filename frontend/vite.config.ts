import path from 'node:path';
import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Dev-only: strip the CSP meta from index.html. In production it is
// defense-in-depth for rendered email HTML, but `script-src 'self'` would
// block @vitejs/plugin-react's inline react-refresh preamble in dev.
function stripCspInDev(): Plugin {
  return {
    name: 'strip-csp-in-dev',
    apply: 'serve',
    transformIndexHtml: (html) =>
      html.replace(/\s*<meta\s+http-equiv="Content-Security-Policy"[\s\S]*?\/>(?=\s*<title>)/, ''),
  };
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss(), stripCspInDev()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:3000',
      '/health': 'http://127.0.0.1:3000',
      '/version': 'http://127.0.0.1:3000',
    },
  },
});
