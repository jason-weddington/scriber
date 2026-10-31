import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { hold, open, press } from './helpers';

// The live input-level waveform below the buttons: bars scroll in from the
// right while recording, freeze while paused, clear on New and Discard, and
// become one level bar under reduced motion. The mock microphone sends a
// level every 50 ms (LEVEL_MS) while recording; the clock is Playwright's,
// paused once the page has loaded so that only runFor moves it (a clock left
// running lets real time add levels, and the exact bar counts flake).

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

async function slots(page: Page): Promise<number> {
  return Number(await page.locator('.wave').getAttribute('data-slots'));
}

/** The mock's level for its nth window (src/lib/mock.ts, syntheticLevel). */
function synthetic(n: number): number {
  return Math.min(1, 0.1 + 0.75 * Math.abs(Math.sin(n * 0.45)) * (0.7 + 0.3 * Math.sin(n * 0.13)));
}

test('the waveform appears while recording, newest bar on the right, as loud as the input', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New');
  await expect(page.locator('.meter')).toHaveCount(0);
  await press(page, 'Record');
  await expect(page.locator('.wave')).toBeVisible();
  await expect(page.locator('.wave')).toHaveAttribute('data-slots', '155');
  await expect(page.locator('.wave .bar')).toHaveCount(155);
  expect(await slots(page)).toBe(155);
  expect(await count(page)).toBe(0);
  await page.clock.runFor(LEVEL_MS * 5);
  await expect.poll(() => count(page)).toBe(5);
  const bars = await drawn(page);
  expect(
    bars.slice(0, 155 - 5).every((l) => l === 0),
    'older slots empty',
  ).toBe(true);
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
  // 4 px bars on a 6 px pitch.
  const boxes = await page.locator('.wave .bar').evaluateAll((els) =>
    els.map((e) => {
      const r = e.getBoundingClientRect();
      return { left: r.left, width: r.width };
    }),
  );
  for (const box of boxes) expect(box.width).toBeCloseTo(4, 1);
  const [prev, last] = boxes.slice(-2);
  expect((last?.left ?? 0) - (prev?.left ?? 0)).toBeCloseTo(6, 1);
});

test('the waveform keeps the newest bars as it scrolls', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await expect(page.locator('.wave')).toHaveAttribute('data-slots', '155');
  await page.clock.runFor(LEVEL_MS * (155 + 10));
  await expect.poll(() => count(page)).toBe(165);
  const bars = await drawn(page);
  expect(bars.at(-1)).toBeCloseTo(synthetic(164), 5);
  expect(bars[0]).toBeCloseTo(synthetic(10), 5);
});

test('a narrow window draws fewer bars', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 640 });
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await expect.poll(() => page.locator('.wave').evaluate((e) => e.clientWidth)).toBe(488);
  await expect(page.locator('.wave')).toHaveAttribute('data-slots', '81');
  await expect(page.locator('.wave .bar')).toHaveCount(81);
});

test('the bars take the recording colour while recording and grey while paused', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await page.clock.runFor(LEVEL_MS * 3);
  await expect(page.locator('.wave .bar').last()).toHaveCSS('background-color', 'rgb(208, 16, 58)');
  await press(page, 'Pause');
  await expect(page.locator('.wave .bar').last()).toHaveCSS('background-color', 'rgb(92, 87, 82)');
});

test('Pause freezes the waveform, Resume carries on, Discard and New clear it', async ({ page }) => {
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
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
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
  await page.clock.runFor(LEVEL_MS * 3);
  await expect(page.locator('.wave')).toBeHidden();
  await expect(page.locator('.level')).toBeVisible();
  const fill = await page
    .locator('.level-fill')
    .evaluate((e) => Number((e as HTMLElement).style.getPropertyValue('--level')));
  expect(fill).toBeCloseTo(synthetic(2), 5);
});

test('the waveform is hidden from screen readers, which hear a status instead', async ({ page }) => {
  await open(page, { libraryOpen: '0' });
  await press(page, 'New', 'Record');
  await expect(page.locator('.wave')).toHaveAttribute('aria-hidden', 'true');
  await expect(page.locator('.level')).toHaveAttribute('aria-hidden', 'true');
  await press(page, 'Pause');
  await expect(page.locator('.meter')).toContainText('The microphone is off while paused.');
});

async function record(page: Page, opts: Record<string, string> = { libraryOpen: '0' }): Promise<void> {
  await page.clock.install({ time: 0 });
  await open(page, opts);
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record');
}

test('the bars take the dark recording colour in dark mode', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await record(page);
  await page.clock.runFor(LEVEL_MS * 3);
  await expect(page.locator('.wave .bar').last()).toHaveCSS('background-color', 'rgb(255, 77, 109)');
});

test('the live time takes the recording colour, grey when paused', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await record(page);
  await expect(page.locator('.time')).toHaveCSS('color', 'rgb(208, 16, 58)');
  await press(page, 'Pause');
  await expect(page.locator('.time')).toHaveCSS('color', 'rgb(92, 87, 82)');
  await press(page, 'Resume');
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(page.locator('.time')).toHaveCSS('color', 'rgb(255, 77, 109)');
});

test('an error stays its own colour, apart from the recording colour', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await open(page, { libraryOpen: '0', transcribe: 'fail' });
  await press(page, 'New', 'Record', 'Done');
  await expect(page.getByRole('button', { name: /Retry/ })).toBeVisible();
  await expect(page.locator('main .error')).toHaveCSS('color', 'rgb(163, 38, 27)');
});

test('the controls stay put when recording starts', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  const top = async (): Promise<number> => (await page.locator('.controls').boundingBox())?.y ?? NaN;
  await press(page, 'New');
  const ready = await top();
  await press(page, 'Record');
  expect(Math.abs((await top()) - ready)).toBeLessThanOrEqual(1);
  await press(page, 'Pause');
  expect(Math.abs((await top()) - ready)).toBeLessThanOrEqual(1);
});

test('the strip is 60% of the free height, centred', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await record(page);
  const w = await page.locator('.wave').boundingBox();
  const m = await page.locator('.meter').boundingBox();
  const content = (await page.locator('.meter').evaluate((e) => e.clientHeight)) - 8;
  if (w === null || m === null) throw new Error('no boxes');
  expect(Math.abs(w.height - 0.6 * content)).toBeLessThanOrEqual(1);
  expect(w.height).toBeGreaterThanOrEqual(170);
  expect(Math.abs(w.y + w.height / 2 - (m.y + m.height / 2))).toBeLessThanOrEqual(2);
});

test('the strip stops growing at 360 px', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1400 });
  await record(page);
  const wave = await page.locator('.wave').boundingBox();
  expect(Math.abs((wave?.height ?? 0) - 360)).toBeLessThanOrEqual(1);
});

test('a full-level bar is as tall as the strip', async ({ page }) => {
  await record(page);
  const sizes = await page
    .locator('.wave .bar')
    .first()
    .evaluate((e) => {
      (e as HTMLElement).style.setProperty('--level', '1');
      return { bar: e.getBoundingClientRect().height, wave: (e.parentElement as HTMLElement).clientHeight };
    });
  expect(Math.abs(sizes.bar - sizes.wave)).toBeLessThanOrEqual(1);
});

test('the strip fits the smallest window', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await record(page, {});
  const wave = await page.locator('.wave').boundingBox();
  expect(wave?.height ?? 0).toBeGreaterThanOrEqual(56);
  const meter = await page.locator('.meter').boundingBox();
  const main = await page.locator('main').boundingBox();
  expect((meter?.y ?? 0) + (meter?.height ?? 0)).toBeLessThanOrEqual((main?.y ?? 0) + (main?.height ?? 0) + 0.5);
});

test('pausing keeps the strip the same size', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await record(page);
  const before = (await page.locator('.meter').boundingBox())?.height ?? 0;
  // The transcript so far shares the space once a segment is heard; hold that back.
  await hold(page, 'transcribe');
  await press(page, 'Pause');
  const after = (await page.locator('.meter').boundingBox())?.height ?? 1;
  expect(Math.abs(before - after)).toBeLessThanOrEqual(1);
});

test('with reduced motion the level bar is 48 px, centred and in the recording colour', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce', colorScheme: 'light' });
  await page.setViewportSize({ width: 960, height: 640 });
  await record(page);
  await page.clock.runFor(LEVEL_MS * 3);
  const level = (await page.locator('.level').boundingBox()) ?? {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
  };
  const meter = (await page.locator('.meter').boundingBox()) ?? {
    x: 0,
    y: 0,
    width: 1,
    height: 1,
  };
  expect(Math.abs(level.height - 48)).toBeLessThanOrEqual(1);
  expect(Math.abs(level.width - meter.width)).toBeLessThanOrEqual(1);
  expect(Math.abs(level.y + level.height / 2 - (meter.y + meter.height / 2))).toBeLessThanOrEqual(2);
  await expect(page.locator('.level-fill')).toHaveCSS('background-color', 'rgb(208, 16, 58)');
  await press(page, 'Pause');
  await expect(page.locator('.level-fill')).toHaveCSS('background-color', 'rgb(92, 87, 82)');
});
