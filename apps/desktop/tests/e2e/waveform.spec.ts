import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { open, press } from './helpers';

// The live input-level waveform above the buttons: bars scroll in from the
// right while recording, freeze while paused, clear on New and Discard, and
// become one level bar under reduced motion. The mock microphone sends a
// level every 50 ms (LEVEL_MS) while recording; the clock is Playwright's,
// paused once the page has loaded so that only runFor moves it (a clock left
// running lets real time add levels, and the exact bar counts flake).

const BARS = 48;
const LEVEL_MS = 50;

/** Each bar's drawn level, oldest first. */
async function drawn(page: Page): Promise<number[]> {
  return page
    .locator('.wave .bar')
    .evaluateAll((bars) => bars.map((b) => Number((b as HTMLElement).style.getPropertyValue('--level'))));
}

async function count(page: Page): Promise<number> {
  return Number(await page.locator('.wave').getAttribute('data-count'));
}

/** The mock's level for its nth window (src/lib/mock.ts, syntheticLevel). */
function synthetic(n: number): number {
  return Math.min(1, 0.1 + 0.55 * Math.abs(Math.sin(n * 0.45)) * (0.7 + 0.3 * Math.sin(n * 0.13)));
}

test('the waveform appears while recording, newest bar on the right, as loud as the input', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page);
  await page.clock.pauseAt(60_000);
  await press(page, 'New');
  await expect(page.locator('.meter')).toHaveCount(0);
  await press(page, 'Record');
  await expect(page.locator('.wave')).toBeVisible();
  await expect(page.locator('.wave .bar')).toHaveCount(BARS);
  expect(await count(page)).toBe(0);
  await page.clock.runFor(LEVEL_MS * 5);
  await expect.poll(() => count(page)).toBe(5);
  const bars = await drawn(page);
  expect(bars.slice(0, BARS - 5).every((l) => l === 0), 'older slots empty').toBe(true);
  const newest = bars.slice(-5);
  newest.forEach((level, n) => {
    expect(level, `bar ${String(n)}`).toBeCloseTo(synthetic(n), 5);
  });
  // A louder level draws a taller bar.
  const heights = await page
    .locator('.wave .bar')
    .evaluateAll((els) => els.slice(-5).map((e) => e.getBoundingClientRect().height));
  const louder = newest.indexOf(Math.max(...newest));
  const quieter = newest.indexOf(Math.min(...newest));
  expect(heights[louder]).toBeGreaterThan(heights[quieter] ?? Infinity);
  await expect(page.locator('.meter')).toContainText('The microphone is picking up sound.');
});

test('the waveform keeps the newest bars as it scrolls', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page);
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await page.clock.runFor(LEVEL_MS * (BARS + 10));
  await expect.poll(() => count(page)).toBe(BARS);
  const bars = await drawn(page);
  expect(bars.at(-1)).toBeCloseTo(synthetic(BARS + 9), 5);
  expect(bars[0]).toBeCloseTo(synthetic(10), 5);
});

test('Pause freezes the waveform, Resume carries on, Discard and New clear it', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page);
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await page.clock.runFor(LEVEL_MS * 6);
  await expect.poll(() => count(page)).toBe(6);
  await press(page, 'Pause');
  const frozen = await drawn(page);
  await page.clock.runFor(LEVEL_MS * 10);
  expect(await drawn(page), 'frozen, not cleared').toEqual(frozen);
  await expect(page.locator('.meter')).toHaveClass(/frozen/);
  await press(page, 'Resume');
  await page.clock.runFor(LEVEL_MS * 2);
  await expect.poll(() => count(page)).toBe(8);
  await press(page, 'Discard');
  await expect(page.locator('.meter')).toHaveCount(0);
  await press(page, 'Record');
  expect(await count(page), 'cleared by Discard').toBe(0);
  await page.clock.runFor(LEVEL_MS * 3);
  await press(page, 'Done');
  await expect(page.locator('.meter')).toHaveCount(0);
  await press(page, 'New', 'Record');
  expect(await count(page), 'cleared by New').toBe(0);
});

test('with reduced motion, one level bar replaces the scrolling waveform', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.clock.install({ time: 0 });
  await open(page);
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await page.clock.runFor(LEVEL_MS * 3);
  await expect(page.locator('.wave')).toBeHidden();
  await expect(page.locator('.level')).toBeVisible();
  const fill = await page.locator('.level-fill').evaluate((e) => Number((e as HTMLElement).style.getPropertyValue('--level')));
  expect(fill).toBeCloseTo(synthetic(2), 5);
});

test('the waveform is hidden from screen readers, which hear a status instead', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await expect(page.locator('.wave')).toHaveAttribute('aria-hidden', 'true');
  await expect(page.locator('.level')).toHaveAttribute('aria-hidden', 'true');
  await press(page, 'Pause');
  await expect(page.locator('.meter')).toContainText('The microphone is off while paused.');
});
