import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// The dashboard is built into dist/web and served by the Node server.
// `npm run dev:web` runs Vite with hot reload and proxies the API to a running server.
export default defineConfig({
  root: 'src/web',
  plugins: [react()],
  build: { outDir: '../../dist/web', emptyOutDir: true },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8080',
      '/preview.mjpg': 'http://127.0.0.1:8080',
    },
  },
});
