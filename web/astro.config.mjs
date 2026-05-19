import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import tailwind from '@astrojs/tailwind';
import path from 'node:path';

export default defineConfig({
  integrations: [
    react({
      // Optimize React integration
      experimentalReact: true,
    }),
    tailwind({
      // Optimize Tailwind CSS
      applyBaseStyles: false,
    }),
  ],
  // Disable HMR to avoid WebSocket issues during development
  devToolbar: {
    enabled: false,
  },
  output: 'static',
  site: 'https://github.com/yourusername/openproxy-rust',
  base: '/',
  compressHTML: true,
  build: {
    format: 'file', // Better for simple routing
    inlineStylesheets: 'auto', // Better for caching
  },
  vite: {
    server: {
      hmr: false,
      // Dev-only: forward backend API + asset routes to the Rust server on :4623
      // so the dashboard works when running `astro dev` on :4624 against a
      // separate `cargo run -- --port 4623` process.
      proxy: {
        '/api': { target: 'http://127.0.0.1:4623', changeOrigin: true },
        '/v1': { target: 'http://127.0.0.1:4623', changeOrigin: true },
        '/health': { target: 'http://127.0.0.1:4623', changeOrigin: true },
        '/oauth': { target: 'http://127.0.0.1:4623', changeOrigin: true },
      },
    },
    resolve: {
      alias: {
        '@': path.resolve('./src'),
      },
    },
    optimizeDeps: {
      include: ['react', 'react-dom', 'react-is'],
    },
    build: {
      // Optimize bundle size
      rollupOptions: {
        output: {
          manualChunks: {
            // Split React libraries
            'react-vendor': ['react', 'react-dom', 'react-is'],
            // Split UI libraries
            'ui-vendor': ['recharts', '@xyflow/react', '@monaco-editor/react'],
            // Split utility libraries
            'utils-vendor': ['zustand', 'lowdb', 'marked'],
          },
        },
      },
      // Enable minification
      minify: 'terser',
      terserOptions: {
        compress: {
          drop_console: true,
          drop_debugger: true,
          pure_funcs: ['console.log', 'console.info'],
        },
        mangle: {
          safari10: true,
        },
      },
    },
  },
});
