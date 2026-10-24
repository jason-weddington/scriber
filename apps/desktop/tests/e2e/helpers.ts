import { expect, type Locator, type Page } from '@playwright/test';

/** Loads the app against the mock with these URL parameters (see src/lib/mock.ts). */
export async function open(page: Page, params: Record<string, string> = {}): Promise<void> {
  await page.goto(`/?${new URLSearchParams(params).toString()}`);
  await expect(page.locator('html')).toHaveAttribute('data-bridge', 'mock');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
}

export function chooseButton(page: Page): Locator {
  return page.getByRole('button', { name: /^Choose (another )?folder…$/ });
}

/** Makes the mock's next calls of `command` wait until `release`. */
export async function hold(page: Page, command: string): Promise<void> {
  await page.evaluate((c) => {
    window.__mockBridge?.hold(c);
  }, command);
}

export async function release(page: Page, command: string): Promise<void> {
  await page.evaluate((c) => {
    window.__mockBridge?.release(c);
  }, command);
}

/** Every path the UI asked the mock to scan. */
export async function scans(page: Page): Promise<string[]> {
  return page.evaluate(() => [...(window.__mockBridge?.scans ?? [])]);
}

/**
 * Every item of a list that overflows its container can be scrolled into view,
 * and none is squeezed below its content height (docs/tauri-design-notes.md,
 * "Layout that survives real data"). Use it on every list.
 */
export async function expectEveryItemReachable(items: Locator): Promise<void> {
  const n = await items.count();
  expect(n, 'the fixture must overflow the container').toBeGreaterThan(10);
  for (const i of [0, n - 1]) {
    const item = items.nth(i);
    await item.scrollIntoViewIfNeeded();
    // 0.9, not 1: sub-pixel heights leave a fully scrolled item a fraction short.
    await expect(item).toBeInViewport({ ratio: 0.9 });
  }
  const squeezed = await items.evaluateAll((els) => els.filter((e) => e.scrollHeight > e.clientHeight + 1).length);
  expect(squeezed, 'items squeezed below their content height').toBe(0);
}
