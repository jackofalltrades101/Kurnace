// Kurnace app frontend.
// The Revideo plugin serves its own editor at "/" and points JSX at Revideo's
// runtime, so our UI lives at /kurnace.html and every React file starts with
// `/** @jsxImportSource react */`.
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import react from '@vitejs/plugin-react';
import revideoPlugin from '@revideo/vite-plugin';
import { defineConfig } from 'vite';

// CJS module: Vite's config bundler may hand us the namespace instead of the default.
const revideo = ((revideoPlugin as unknown as { default?: typeof revideoPlugin }).default ?? revideoPlugin);
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
    // Library media, same-origin with the page (see asset server in src-tauri/src/main.rs).
    proxy: { '/kurn-asset': { target: 'http://127.0.0.1:1421', rewrite: p => p.replace(/^\/kurn-asset/, '') } },
  },
  build: {
    // Tauri opens index.html; it redirects to the real UI at kurnace.html.
    // Both are explicit inputs so the Revideo project and its chunks are
    // bundled for production, not just the dev server.
    rollupOptions: {
      input: {
        index: path.join(root, 'index.html'),
        kurnace: path.join(root, 'kurnace.html'),
      },
    },
  },
  // Build-time defaults for the shared Revideo project; the app overrides size
  // and fps per timeline at runtime (src/preview.tsx).
  define: {
    __KURN_FPS__: '30',
    __KURN_WIDTH__: '1920',
    __KURN_HEIGHT__: '1080',
    __KURN_SCALE__: '1',
  },
  plugins: [react(), revideo({ project: path.join(root, '../modules/render.revideo/src/project.ts') })],
});
