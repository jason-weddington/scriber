import { defineConfig, devices } from '@playwright/test';

// Chromium only: the point of this suite is that UI work never needs the Tauri
// runtime, a webview or a Mac. The webServer block starts Vite itself, so the
// runner is self-contained.
//
// Two projects. `unit` exercises pure modules (format, the mock's rules, the
// CSP string) with no page; `e2e` drives the real screens in a browser
// against the mock bridge. A unit failure reads as "the rule is wrong" rather
// than as a flaky browser run.
export default defineConfig({
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: 'list',
  // Only active for `pnpm test:coverage` (see tests/coverage/).
  globalSetup: './tests/coverage/global-setup.ts',
  globalTeardown: './tests/coverage/global-teardown.ts',
  use: {
    baseURL: 'http://localhost:1430',
  },
  webServer: {
    // Its own port, never reused: a `pnpm tauri dev` on 1420 serves whichever
    // checkout started it, and reusing it would test the wrong code.
    command: 'pnpm exec vite --port 1430 --strictPort',
    url: 'http://localhost:1430',
    reuseExistingServer: false,
    timeout: 120_000,
  },
  projects: [
    {
      name: 'unit',
      testDir: './tests/unit',
    },
    {
      name: 'e2e',
      testDir: './tests/e2e',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 960, height: 640 },
        timezoneId: 'UTC',
        locale: 'en-US',
        colorScheme: 'light',
      },
    },
  ],
});
