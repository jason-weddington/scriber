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
  for (const name of ['New', 'Copy', 'Done']) {
    await expect(button(page, name)).toBeInViewport({ ratio: 1 });
  }
  await expect(transcript).toBeInViewport();
  const fits = await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight);
  expect(fits, 'the window itself never scrolls').toBe(true);
});
