import { defineConfig, devices } from '@playwright/test';

const PORT = 4173;

/**
 * Playwright setup for the dashboard (closes out GitHub issue #63, which the
 * old `test` script pointed at as a placeholder).
 *
 * Tests run against a production build served by `vite preview` rather than
 * the dev server, so what CI exercises is the same bundle that ships. The
 * `webServer` block builds and starts it automatically.
 */
export default defineConfig({
  testDir: './e2e',
  // A route smoke test that hangs is a failure, not something to wait out.
  timeout: 30_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  // Fail the build if a `test.only` is committed.
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : [['list']],

  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },

  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],

  webServer: {
    command: `pnpm run build && pnpm run preview --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
