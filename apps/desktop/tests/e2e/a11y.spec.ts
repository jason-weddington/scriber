import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { chooseButton, hold, open, release } from './helpers';

// axe-core over every state the window can be in, in light and dark. Any
// violation fails, whatever its impact: a contrast miss in dark mode is as
// real as a missing label.

async function expectNoViolations(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help} (${String(v.nodes.length)})`)).toEqual([]);
}

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`${colorScheme} mode`, () => {
    test.use({ colorScheme });

    test('idle', async ({ page }) => {
      await open(page);
      await expectNoViolations(page);
    });

    test('scanning', async ({ page }) => {
      await open(page);
      await hold(page, 'scan_folder');
      await chooseButton(page).click();
      await expect(chooseButton(page)).toBeDisabled();
      await expectNoViolations(page);
      await release(page, 'scan_folder');
    });

    test('summary', async ({ page }) => {
      await open(page);
      await chooseButton(page).click();
      await expect(page.getByRole('list', { name: 'File types' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('error', async ({ page }) => {
      await open(page, { tree: 'locked' });
      await chooseButton(page).click();
      await expect(page.getByRole('alert')).toBeVisible();
      await expectNoViolations(page);
    });

    test('empty', async ({ page }) => {
      await open(page, { tree: 'empty' });
      await chooseButton(page).click();
      await expect(page.getByText('This folder has no files.')).toBeVisible();
      await expectNoViolations(page);
    });
  });
}
