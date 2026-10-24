import { expect, test } from './fixtures';

import { button, hold, open, press, release, status } from './helpers';

// A missing speech model downloads at launch. While it does, Record cannot
// work, so the window shows a determinate progress bar and keeps Record off;
// once it is in place Record turns on with no notice. A failed download is
// an error with Retry download. The mock downloads in 4 steps
// (?model=download), and under ?model=fail its first download fails.

test('the download shows its progress, keeps Record off, then gets out of the way', async ({ page }) => {
  await open(page, { model: 'download', hold: 'download' });
  await press(page, 'New');
  const progress = page.getByRole('progressbar');
  await expect(progress).toHaveAttribute('value', '0');
  await expect(page.locator('.download-text')).toHaveText(
    'Downloading the speech model, needed once: 0% (0 B of 1.6 GB). Record works when it is done.',
  );
  await expect(button(page, 'Record')).toBeDisabled();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await release(page, 'download');
  await expect(progress).toHaveCount(0);
  await expect(button(page, 'Record')).toBeEnabled();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await expect(status(page)).toHaveText('Click Record and start talking.');
});

test('the progress moves step by step', async ({ page }) => {
  await open(page, { model: 'download', hold: 'download' });
  await expect(page.getByRole('progressbar')).toHaveAttribute('value', '0');
  await release(page, 'download');
  await hold(page, 'download');
  await expect(page.getByRole('progressbar')).toHaveAttribute('value', '24');
  await expect(page.locator('.download-text')).toContainText('24% (406 MB of 1.6 GB)');
  await release(page, 'download');
});

test('a failed download says why and offers Retry download, which then succeeds', async ({ page }) => {
  await open(page, { model: 'fail' });
  await expect(page.getByRole('alert')).toHaveText('Could not download the speech model: the connection was reset.');
  await expect(page.getByRole('progressbar')).toHaveCount(0);
  await press(page, 'New');
  await expect(button(page, 'Record')).toBeDisabled();
  await press(page, 'Retry download');
  await expect(page.getByRole('alert')).toHaveCount(0);
  await expect(button(page, 'Retry download')).toHaveCount(0);
  await expect(button(page, 'Record')).toBeEnabled();
});
