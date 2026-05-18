import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'path';
import { config as dotenvConfig } from 'dotenv';

dotenvConfig({ path: resolve(__dirname, '.env') });

const sentryDefines = {
  __SENTRY_DSN__: JSON.stringify(process.env.SENTRY_DSN || ''),
  __SENTRY_TRACES_SAMPLE_RATE__: JSON.stringify(process.env.SENTRY_TRACES_SAMPLE_RATE || '0.1'),
  __SENTRY_PROFILES_SAMPLE_RATE__: JSON.stringify(process.env.SENTRY_PROFILES_SAMPLE_RATE || '0.1'),
};

export default defineConfig({
  root: resolve(__dirname, 'src/renderer'),
  define: sentryDefines,
  plugins: [react()],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src/renderer'),
      '@shared': resolve(__dirname, 'src/shared'),
      '@features': resolve(__dirname, 'src/renderer/features'),
      '@components': resolve(__dirname, 'src/renderer/shared/components'),
      '@hooks': resolve(__dirname, 'src/renderer/shared/hooks'),
      '@lib': resolve(__dirname, 'src/renderer/shared/lib'),
    },
  },
  server: {
    port: 5174,
    strictPort: true,
    watch: {
      ignored: [
        '**/node_modules/**',
        '**/.git/**',
        '**/.worktrees/**',
        '**/.auto-claude/**',
        '**/out/**',
        '**/out-tauri/**',
        '**/src-tauri/target/**',
      ],
    },
  },
  build: {
    outDir: resolve(__dirname, 'out-tauri/renderer'),
    emptyOutDir: true,
    rollupOptions: {
      input: resolve(__dirname, 'src/renderer/index.html'),
    },
  },
  clearScreen: false,
});
