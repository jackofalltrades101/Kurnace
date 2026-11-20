// Builds the headless render page (dist/render.html + assets). The app consumes
// src/compositor.ts directly through its own Vite build.
import { defineConfig } from 'vite';

export default defineConfig({
  base: './',
  build: { outDir: 'dist', emptyOutDir: true, target: 'es2022', rollupOptions: { input: 'render.html' } },
});
