import { expect, test } from './fixtures';

import { button, open, press } from './helpers';

// The one container that holds a variable amount of content is the
// transcript. Filled far past the window at its minimum size, it scrolls
// inside itself and the controls stay in view.
test('a long transcript scrolls inside its box and the controls stay put', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await open(page, { long: '1' });
  await press(page, 'New', 'Record', 'Pause', 'Resume', 'Pause', 'Resume', 'Done');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toHaveValue(/part 3 of the dictation\.$/);
  const overflow = await transcript.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(overflow, 'the text overflows the box').toBeGreaterThan(100);
  for (const name of ['New', 'Copy', 'Discard']) {
    await expect(button(page, name)).toBeInViewport({ ratio: 1 });
  }
  await expect(transcript).toBeInViewport();
  const fits = await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight);
  expect(fits, 'the window itself never scrolls').toBe(true);
});

// With the library drawer open (the default) at the minimum window size, the
// recorder takes the rest of the width: its controls wrap and the transcript
// shrinks, and neither way does the window scroll.
test('at the minimum size with the drawer open the controls stay in view and nothing scrolls the window', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await open(page, { library: '30', long: '1' });
  await expect(page.getByRole('navigation', { name: 'Recordings' })).toBeVisible();
  await press(page, 'New', 'Record', 'Pause', 'Resume', 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/part 2 of the dictation\.$/);
  for (const name of ['New', 'Copy', 'Discard']) {
    await expect(button(page, name)).toBeInViewport({ ratio: 1 });
  }
  const fits = await page.evaluate(
    () =>
      document.documentElement.scrollHeight <= window.innerHeight && document.documentElement.scrollWidth <= window.innerWidth,
  );
  expect(fits, 'no vertical or horizontal window scroll').toBe(true);
});

test('Copy sits inside the box, clear of the text, and stays put while the text scrolls', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await open(page, { long: '1' });
  await press(page, 'New', 'Record', 'Pause', 'Resume', 'Pause', 'Resume', 'Done');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toHaveValue(/part 3 of the dictation\.$/);
  expect(await transcript.evaluate((el) => el.scrollHeight - el.clientHeight)).toBeGreaterThan(100);
  const box = await transcript.boundingBox();
  const copy = await button(page, 'Copy').boundingBox();
  if (box === null || copy === null) throw new Error('no boxes');
  expect(copy.x).toBeGreaterThanOrEqual(box.x);
  expect(copy.y).toBeGreaterThanOrEqual(box.y);
  expect(copy.x + copy.width).toBeLessThanOrEqual(box.x + box.width);
  expect(copy.y + copy.height).toBeLessThanOrEqual(box.y + box.height);
  const textRight = await transcript.evaluate((el) => {
    const rect = el.getBoundingClientRect();
    return rect.left + el.clientLeft + el.clientWidth - parseFloat(getComputedStyle(el).paddingRight);
  });
  expect(copy.x).toBeGreaterThanOrEqual(textRight);
  await transcript.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  expect(await transcript.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  expect(await button(page, 'Copy').boundingBox()).toEqual(copy);
});
