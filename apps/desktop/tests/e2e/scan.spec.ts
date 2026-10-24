import { expect, test } from './fixtures';

import { chooseButton, hold, open, release, scans } from './helpers';

// The example feature end to end, against the mock bridge: pick a folder,
// watch progress, read the summary.

const ROOT = '/Users/me/Pictures';

test('the idle window invites a choice and shows the mock version', async ({ page }) => {
  await open(page);
  await expect(page.getByText('Choose a folder to see what is in it, by file type.')).toBeVisible();
  await expect(chooseButton(page)).toHaveText('Choose folder…');
  await expect(page.getByText('Version 0.0.0-mock')).toBeVisible();
});

test('a scan lists every file type, largest first', async ({ page }) => {
  await open(page);
  await chooseButton(page).click();
  await expect(page.getByText(`6 files, 58 MB, in ${ROOT}.`)).toBeVisible();
  const rows = page.getByRole('list', { name: 'File types' }).getByRole('listitem');
  await expect(rows).toHaveText([
    /^\.dng\s*2 files\s*50 MB\s*$/,
    /^\.jpg\s*2 files\s*8\.1 MB\s*$/,
    /^No extension\s*1 file\s*6\.1 kB\s*$/,
    /^\.txt\s*1 file\s*1\.2 kB\s*$/,
  ]);
  // The share bars are sized at run time, relative to the largest row.
  await expect(rows.first().locator('.fill')).toHaveCSS('width', /px$/);
  expect(await rows.first().locator('.fill').evaluate((el) => el.style.width)).toBe('100%');
  await expect(page.getByText('1 folder could not be read, so the totals leave them out.')).toBeVisible();
  await expect(chooseButton(page)).toHaveText('Choose another folder…');
  expect(await scans(page)).toEqual([ROOT]);
});

test('progress shows while the scan runs, and the button waits for it', async ({ page }) => {
  await open(page);
  await hold(page, 'scan_folder');
  await chooseButton(page).click();
  await expect(page.getByText(`Scanning ${ROOT}: 6 files, 58 MB so far…`)).toBeVisible();
  await expect(chooseButton(page)).toBeDisabled();
  await release(page, 'scan_folder');
  await expect(page.getByText(`6 files, 58 MB, in ${ROOT}.`)).toBeVisible();
  await expect(chooseButton(page)).toBeEnabled();
});

test('cancelling the picker changes nothing', async ({ page }) => {
  await open(page, { pick: 'cancel' });
  await chooseButton(page).click();
  await expect(page.getByText('Choose a folder to see what is in it, by file type.')).toBeVisible();
  expect(await scans(page)).toEqual([]);
});

test('an unreadable folder is reported in one sentence', async ({ page }) => {
  await open(page, { tree: 'locked' });
  await chooseButton(page).click();
  await expect(page.getByRole('alert')).toHaveText(`${ROOT} could not be read: Permission denied (os error 13)`);
  await expect(chooseButton(page)).toBeEnabled();
});

test('an empty folder says so', async ({ page }) => {
  await open(page, { tree: 'empty' });
  await chooseButton(page).click();
  await expect(page.getByText(`0 files, 0 B, in ${ROOT}.`)).toBeVisible();
  await expect(page.getByText('This folder has no files.')).toBeVisible();
  await expect(page.getByText(/could not be read/)).toHaveCount(0);
});

test('the window still works when the version cannot be read', async ({ page }) => {
  await open(page, { fail: 'app_info' });
  await chooseButton(page).click();
  await expect(page.getByText(`6 files, 58 MB, in ${ROOT}.`)).toBeVisible();
  await expect(page.getByText(/^Version/)).toHaveCount(0);
});
