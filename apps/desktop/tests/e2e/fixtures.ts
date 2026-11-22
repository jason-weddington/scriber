// The e2e `test`: Playwright's, plus an automatic fixture that collects the
// page's V8 coverage when `pnpm test:coverage` sets COVERAGE. Specs import
// `test` and `expect` from here. For built-web chunks served from a test
// dist directory, adjacent `.map` files are attached so Monocart remaps them
// back to canonical `src/` modules.

import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import { expect, test as base } from '@playwright/test';
import MCR from 'monocart-coverage-reports';

import { coverageOptions, WEB_CHUNK_URL } from '../coverage/options';

function attachWebSourceMaps<T extends { url: string }>(entries: T[]): (T & { sourceMap?: unknown })[] {
  const distDir = process.env.SCRIBER_WEB_TEST_DIST ?? resolve(process.cwd(), 'dist-web');
  return entries.map((entry) => {
    if (!WEB_CHUNK_URL.test(entry.url)) return entry;
    try {
      const pathname = new URL(entry.url).pathname.replace(/^\/+/, '');
      const mapPath = join(distDir, `${pathname}.map`);
      if (!existsSync(mapPath)) return entry;
      const sourceMap = JSON.parse(readFileSync(mapPath, 'utf8')) as unknown;
      return { ...entry, sourceMap };
    } catch {
      return entry;
    }
  });
}

export const test = base.extend<{ collectCoverage: undefined }>({
  collectCoverage: [
    async ({ page }, use) => {
      if (!process.env.COVERAGE) {
        await use(undefined);
        return;
      }
      await page.coverage.startJSCoverage({ resetOnNavigation: false });
      await use(undefined);
      const js = attachWebSourceMaps(await page.coverage.stopJSCoverage());
      await MCR(coverageOptions).add(js);
    },
    { auto: true },
  ],
});

export { expect };
