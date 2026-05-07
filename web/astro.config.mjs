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
  output: 'static',
  site: 'https://github.com/yourusername/openproxy-rust',
  base: '/',
  compressHTML: true,
  build: {
    format: 'directory', // Better for static sites caching
    inlineStylesheets: 'auto', // Better for caching
  },
  vite: {
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
