import { expect, test } from './fixtures';

import { button, open } from './helpers';

// The top band: no painted heading, controls aligned, the inset that clears
// the traffic lights windowed and collapses in full screen, and a real gear.

const inset = (page: import('@playwright/test').Page, px: number) => expect(page.locator('.band')).toHaveCSS('padding-left', `${String(px)}px`);

async function x(page: import('@playwright/test').Page, name: string): Promise<number | undefined> {
  return (await button(page, name).boundingBox())?.x;
}

test('the heading stays for assistive tech but paints nothing', async ({ page }) => {
  await open(page);
  const box = await page.getByRole('heading', { level: 1 }).boundingBox();
  expect(box?.width).toBeLessThanOrEqual(1);
  expect(box?.height).toBeLessThanOrEqual(1);
});

test('a drag spacer fills the band and the right controls stay put', async ({ page }) => {
  await open(page);
  const spacer = page.locator('.band-spacer');
  await expect(spacer).toHaveAttribute('data-tauri-drag-region', '');
  const box = await spacer.boundingBox();
  expect(box?.width).toBeGreaterThan(0);
  const band = await page.locator('.band').evaluate((e) => e.clientHeight);
  expect(box?.height).toBe(band);
  expect(band).toBe(51);
  const settings = await button(page, 'Settings').boundingBox();
  const fresh = await button(page, 'New').boundingBox();
  expect((settings?.x ?? 0) + (settings?.width ?? 0)).toBe(944);
  expect((fresh?.x ?? 0) + (fresh?.width ?? 0) + 12).toBe(settings?.x);
});

test('windowed, the Library toggle clears the traffic lights', async ({ page }) => {
  await open(page);
  await inset(page, 96);
  expect(await x(page, 'Library')).toBe(96);
});

test('in full screen it sits at the left edge', async ({ page }) => {
  await open(page, { fullscreen: '1' });
  await inset(page, 16);
  expect(await x(page, 'Library')).toBe(16);
});

test('it moves live when full screen starts and ends', async ({ page }) => {
  await open(page);
  await inset(page, 96);
  await page.evaluate(() => window.__mockBridge?.setFullscreen(true));
  await inset(page, 16);
  expect(await x(page, 'Library')).toBe(16);
  await page.evaluate(() => window.__mockBridge?.setFullscreen(false));
  await inset(page, 96);
  expect(await x(page, 'Library')).toBe(96);
});

test('an unreadable full-screen state keeps the windowed inset, silently', async ({ page }) => {
  const errors: Error[] = [];
  page.on('pageerror', (e) => errors.push(e));
  await open(page, { fail: 'window_fullscreen' });
  await page.evaluate(() => new Promise((r) => setTimeout(r)));
  await inset(page, 96);
  await expect(page.locator('.band [role=alert]')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('Settings is a gear: one filled path, no circle', async ({ page }) => {
  await open(page);
  const settings = button(page, 'Settings');
  await expect(settings.locator('svg circle')).toHaveCount(0);
  await expect(settings.locator('svg path')).toHaveCount(1);
  await expect(settings.locator('svg path')).toHaveAttribute('fill-rule', 'evenodd');
});
