import { expect, test } from './fixtures';

import { chooseButton, expectEveryItemReachable, open } from './helpers';

// Every container that holds a variable number of items, filled past the
// window (tree=many: 40 file types) at the window's minimum size.
test('the file-type list scrolls and every row stays whole', async ({ page }) => {
  await page.setViewportSize({ width: 640, height: 420 });
  await open(page, { tree: 'many' });
  await chooseButton(page).click();
  const rows = page.getByRole('list', { name: 'File types' }).getByRole('listitem');
  await expect(rows).toHaveCount(40);
  await expectEveryItemReachable(rows);
  // The band and its button stay put while the list scrolls under them.
  await expect(chooseButton(page)).toBeInViewport({ ratio: 1 });
});
