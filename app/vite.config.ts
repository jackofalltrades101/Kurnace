// Kurnace app frontend. The UI lives at /kurnace.html (index.html redirects there).
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

const root = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root,
  clearScreen: false,
  // Excalidraw is prebundled separately from the app; force one React copy or
  // hooks run against a null dispatcher.
  resolve: { dedupe: ['react', 'react-dom'] },
  server: {
    port: 1420,
    strictPort: true,
    // Media, exact frames and scene files, same-origin with the page
    // (asset server in src-tauri/src/main.rs).
    proxy: { '/kurn-asset': { target: 'http://127.0.0.1:1421', rewrite: p => p.replace(/^\/kurn-asset/, '') } },
  },
  build: {
    rollupOptions: {
      input: {
        index: path.join(root, 'index.html'),
        kurnace: path.join(root, 'kurnace.html'),
      },
    },
  },
  plugins: [react()],
});
