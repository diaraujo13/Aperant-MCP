// Smoke tests for the Tauri renderer production bundle.
//
// These run against `vite preview` serving `out-tauri/renderer/`, NOT against
// a real Tauri webview. The goal is to catch packaging-level regressions
// (missing chunks, broken HTML, asset 404s) cheaply on every commit, before
// the slower full-bundle CI runs `tauri build`.

import { expect, test } from '@playwright/test';

test.describe('Tauri renderer production bundle', () => {
  test('serves index.html with expected title and root mount point', async ({ page }) => {
    await page.goto('/');
    await expect(page).toHaveTitle('Aperant-MCP');
    await expect(page.locator('#root')).toBeAttached();
  });

  test('main JS bundle and CSS load without 404', async ({ page }) => {
    const failures: Array<{ url: string; status: number }> = [];
    page.on('response', (response) => {
      const url = response.url();
      const status = response.status();
      if (status >= 400 && /\/assets\/.+\.(js|css)$/.test(url)) {
        failures.push({ url, status });
      }
    });

    await page.goto('/', { waitUntil: 'networkidle' });
    expect(failures, `asset 404s: ${JSON.stringify(failures)}`).toHaveLength(0);
  });

  test('bundle does not throw before Tauri shim mounts', async ({ page }) => {
    // The shim is no-op without `__TAURI_INTERNALS__`, which means the renderer
    // will start trying to use `window.electronAPI` and may log errors. We only
    // fail on uncaught JS errors that crash the page (TypeError, ReferenceError).
    const hardErrors: string[] = [];
    page.on('pageerror', (err) => {
      // Filter expected "electronAPI undefined" noise — that's the documented
      // behavior when running outside Tauri without a shim mock.
      const msg = err.message || String(err);
      if (!/electronAPI/i.test(msg)) {
        hardErrors.push(msg);
      }
    });

    await page.goto('/', { waitUntil: 'domcontentloaded' });
    // Give the bundle a moment to evaluate top-level imports.
    await page.waitForTimeout(500);

    expect(hardErrors, `unexpected page errors: ${hardErrors.join('\n')}`).toHaveLength(0);
  });

  test('CSP meta tag is present and includes tauri scheme', async ({ page }) => {
    await page.goto('/');
    const csp = await page
      .locator('meta[http-equiv="Content-Security-Policy"]')
      .getAttribute('content');
    expect(csp).toBeTruthy();
    expect(csp).toMatch(/tauri:/);
    expect(csp).toMatch(/ipc:/);
  });
});
