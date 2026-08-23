import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin, type ViteDevServer } from 'vite';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const LOCAL_BACKEND = {
  target: 'http://127.0.0.1:3000',
  changeOrigin: true,
} as const;

/**
 * Vite's default `localhost` bind is IPv6-only (`[::1]`) on Windows, so
 * `http://127.0.0.1:5173` fails while `http://localhost:5173` works.
 * Mirror the listener onto the other loopback so both URLs work.
 */
function listenOnBothLoopbacks(): Plugin {
  let extra: http.Server | undefined;
  return {
    name: 'lyra-both-loopbacks',
    configureServer(server: ViteDevServer) {
      const httpServer = server.httpServer;
      if (!httpServer) return;
      httpServer.once('listening', () => {
        const addr = httpServer.address();
        if (!addr || typeof addr === 'string') return;
        const extraHost =
          addr.address === '::1' ? '127.0.0.1' : addr.address === '127.0.0.1' ? '::1' : null;
        if (!extraHost) return;
        extra = http.createServer((req, res) => {
          httpServer.emit('request', req, res);
        });
        extra.on('upgrade', (req, socket, head) => {
          httpServer.emit('upgrade', req, socket, head);
        });
        extra.listen(addr.port, extraHost);
      });
      httpServer.once('close', () => extra?.close());
    },
  };
}

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
  plugins: [react(), tailwindcss(), stripCspInDev(), listenOnBothLoopbacks()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    proxy: {
      // Prefix match: covers `/api/v1/...` as well as any future `/api/...`.
      '/api': LOCAL_BACKEND,
      '/health': LOCAL_BACKEND,
      '/version': LOCAL_BACKEND,
    },
  },
});
