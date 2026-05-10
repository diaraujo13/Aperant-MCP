// Playwright config for the Tauri renderer.
//
// Strategy: serve the production bundle (apps/frontend/out-tauri/renderer)
// via `vite preview` and run smoke tests against it in Chromium. This
// validates the bundle is loadable end-to-end without needing a real Tauri
// webview or tauri-driver.
//
// What this catches: 404s on assets, broken HTML, hard JS errors at boot
// before the Tauri shim mounts. What it does NOT catch: real Tauri IPC
// behavior (covered by the Rust integration tests + electron-shim unit tests).

import { defineConfig } from '@playwright/test';

const PORT = 4174;

export default defineConfig({
  testDir: '.',
  testMatch: '**/*.tauri.spec.ts',
  timeout: 30_000,
  expect: { timeout: 5_000 },
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  webServer: {
    // Build the renderer first, then serve it.
    command: `npm run build:tauri-renderer && npx vite preview --config vite.tauri.config.ts --port ${PORT} --strictPort`,
    cwd: '..',
    url: `http://localhost:${PORT}`,
    timeout: 180_000,
    reuseExistingServer: !process.env.CI,
    stdout: 'pipe',
    stderr: 'pipe',
  },
  projects: [{ name: 'tauri-bundle', testMatch: '**/*.tauri.spec.ts' }],
});
