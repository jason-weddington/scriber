import { expect, test } from './fixtures';

import { button, copied, open, press } from './helpers';

// Hover and pressed states: a veil over every enabled button, except the
// selected library row and any disabled button.

const acts = (page: import('@playwright/test').Page): Promise<unknown[]> =>
  page.evaluate(() => [...(window.__mockBridge?.acts ?? [])]);

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`${colorScheme} mode`, () => {
    test.use({ colorScheme });

    test('every enabled button shows a hover and a pressed veil', async ({ page }) => {
      await open(page, { library: '2' });
      const classes = new Set<string>();

      async function walk(selector: string): Promise<void> {
        const before = await acts(page);
        const all = page.locator(selector);
        const n = await all.count();
        for (let i = 0; i < n; i++) {
          const el = all.nth(i);
          classes.add((await el.getAttribute('class')) ?? '');
          await el.hover();
          await expect(el).not.toHaveCSS('background-image', 'none');
          const hover = await el.evaluate((e) => getComputedStyle(e).backgroundImage);
          await page.mouse.down();
          const pressed = await el.evaluate((e) => getComputedStyle(e).backgroundImage);
          expect(pressed).not.toBe(hover);
          await page.mouse.move(0, 0);
          await page.mouse.up();
        }
        expect(await acts(page)).toEqual(before);
      }

      await walk('button:enabled:visible');
      await press(page, 'New');
      await walk('button:enabled:visible');
      await press(page, 'Record');
      await walk('button:enabled:visible');
      await press(page, 'Done');
      await expect(button(page, 'Copy')).toBeEnabled();
      await walk('button:enabled:visible');
      expect(await copied(page)).toHaveLength(1);
      await button(page, 'Settings').click();
      await walk('#settings button:enabled:visible');
      await expect(page.locator('#settings')).toBeVisible();
      expect([...classes].some((c) => c.split(' ').includes('primary'))).toBe(true);
      expect([...classes].some((c) => c.split(' ').includes('quiet'))).toBe(true);
    });

    test('a disabled button shows no veil', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record', 'Done');
      await expect(button(page, 'Copy')).toBeEnabled();
      await page.getByRole('textbox', { name: 'Transcript' }).fill('');
      await button(page, 'Copy').hover({ force: true });
      await expect(button(page, 'Copy')).toHaveCSS('background-image', 'none');
    });

    test('the selected library row shows no veil', async ({ page }) => {
      await open(page, { library: '2' });
      const rows = page.locator('#library li');
      await rows.nth(0).getByRole('button').click();
      const selected = page.locator('#library li button[aria-current="true"]');
      await rows.nth(1).getByRole('button').hover();
      await expect(rows.nth(1).getByRole('button')).not.toHaveCSS('background-image', 'none');
      await selected.hover();
      await expect(selected).toHaveCSS('background-image', 'none');
      await page.mouse.down();
      await expect(selected).toHaveCSS('background-image', 'none');
      await page.mouse.move(0, 0);
      await page.mouse.up();
    });

    test('the checked Copy is green', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record', 'Done', 'Copy');
      await expect(page.locator('button.copy.copied')).toHaveCSS(
        'color',
        colorScheme === 'light' ? 'rgb(26, 127, 55)' : 'rgb(63, 185, 80)',
      );
    });
  });
}
