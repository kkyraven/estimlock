import { resolve } from 'node:path';
import { defineConfig } from 'vite';

const ui = resolve(import.meta.dirname, 'ui');

export default defineConfig({
  root: ui,
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: resolve(import.meta.dirname, 'dist'),
    emptyOutDir: true,
    target: 'es2022',
    rollupOptions: {
      input: { main: resolve(ui, 'main.html'), widget: resolve(ui, 'widget.html') },
    },
  },
});
