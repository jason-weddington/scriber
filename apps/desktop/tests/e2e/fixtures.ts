// The e2e `test`: Playwright's, plus an automatic fixture that collects the
// page's V8 coverage when `pnpm test:coverage` sets COVERAGE. Specs import
// `test` and `expect` from here.

import { expect, test as base } from '@playwright/test';
import MCR from 'monocart-coverage-reports';

import { coverageOptions } from '../coverage/options';

export const test = base.extend<{ collectCoverage: undefined }>({
  collectCoverage: [
    async ({ page }, use) => {
      if (!process.env.COVERAGE) {
        await use(undefined);
        return;
      }
      await page.coverage.startJSCoverage({ resetOnNavigation: false });
      await use(undefined);
      const js = await page.coverage.stopJSCoverage();
      await MCR(coverageOptions).add(js);
    },
    { auto: true },
  ],
});

export { expect };
