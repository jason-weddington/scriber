import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, copied, open, press, release, status } from './helpers';

// The settings panel's vocabulary file: its path (the default one in the
// data folder until another is picked), Browse… (the native open panel,
// answered by the mock's ?pick=) and Open in Text Editor; then the
// post-processing switch, which cleans up each transcript with the local model.

const DEFAULT = '/mock/data/vocabulary.txt';
const PICKED = '/mock/Documents/terms.txt';

function path(page: Page) {
  return page.getByRole('textbox', { name: 'Vocabulary file' });
}

function browse(page: Page) {
  return page.getByRole('button', { name: 'Browse…' });
}

function editor(page: Page) {
  return page.getByRole('button', { name: 'Open in Text Editor' });
}

function alert(page: Page) {
  return page.getByRole('region', { name: 'Settings' }).getByRole('alert');
}

/** Every settings_set call the mock received. */
async function saved(page: Page): Promise<unknown[]> {
  return page.evaluate(() => [...(window.__mockBridge?.saved ?? [])]);
}

async function opened(page: Page): Promise<string[]> {
  return page.evaluate(() => [...(window.__mockBridge?.opened ?? [])]);
}

async function openSettings(page: Page, params: Record<string, string> = {}): Promise<void> {
  await open(page, params);
  await button(page, 'Settings').click();
  await expect(path(page)).toBeVisible();
}

test('the vocabulary file shows the default path at launch', async ({ page }) => {
  await openSettings(page);
  await expect(path(page)).toHaveValue(DEFAULT);
  await expect(path(page)).toHaveAttribute('readonly', '');
  await expect(path(page)).toHaveAttribute('title', DEFAULT);
  await expect(page.getByRole('group', { name: 'Vocabulary file' })).toBeVisible();
});

test('Browse picks another file and saves it', async ({ page }) => {
  await openSettings(page, { pick: PICKED });
  await browse(page).click();
  await expect(path(page)).toHaveValue(PICKED);
  expect(await page.evaluate(() => [...(window.__mockBridge?.browsed ?? [])]), 'the panel starts at the current file').toEqual([DEFAULT]);
  await expect.poll(async () => (await saved(page)).at(-1)).toEqual({ summaries: true, libraryOpen: true, vocabulary: PICKED, cleanup: false });
  await expect(alert(page)).toHaveCount(0);
});

test('a cancelled Browse changes nothing', async ({ page }) => {
  await openSettings(page, { pick: 'cancel' });
  await browse(page).click();
  await editor(page).click();
  await expect.poll(() => opened(page)).toEqual([DEFAULT]);
  await expect(path(page)).toHaveValue(DEFAULT);
  expect(await saved(page)).toEqual([]);
});

test('an open panel that fails says why and saves nothing', async ({ page }) => {
  await openSettings(page, { fail: 'pick_vocabulary_file' });
  await browse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: pick_vocabulary_file');
  await expect(path(page)).toHaveValue(DEFAULT);
  expect(await saved(page)).toEqual([]);
});

test('a picked file that cannot be saved stays on screen and says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set', pick: PICKED });
  await browse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(path(page)).toHaveValue(PICKED);
});

test('Open in Text Editor opens the vocabulary file', async ({ page }) => {
  await openSettings(page);
  await editor(page).click();
  await expect.poll(() => opened(page)).toEqual([DEFAULT]);
  await expect(alert(page)).toHaveCount(0);
});

test('a text editor that fails says why', async ({ page }) => {
  await openSettings(page, { fail: 'vocabulary_open' });
  await editor(page).click();
  await expect(alert(page)).toHaveText('Could not open the vocabulary file: no text editor.');
  expect(await opened(page)).toEqual([]);
});

test('a long path never scrolls the window sideways at the minimum size', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  const long = `/${'a'.repeat(195)}.txt`;
  await openSettings(page, { pick: long });
  await browse(page).click();
  await expect(path(page)).toHaveValue(long);
  await expect(editor(page)).toBeInViewport();
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);
  expect(fits, 'no horizontal scroll').toBe(true);
});

function cleanupSwitch(page: Page) {
  return page.getByRole('switch', { name: 'Enable post-processing with local LLM' });
}

test('post-processing starts off and turning it on saves it', async ({ page }) => {
  await openSettings(page);
  await expect(cleanupSwitch(page)).not.toBeChecked();
  await cleanupSwitch(page).click();
  await expect(cleanupSwitch(page)).toBeChecked();
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: true });
  await expect(alert(page)).toHaveCount(0);
});

test('a post-processing switch that cannot be saved says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await cleanupSwitch(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
});

test('with post-processing on, the transcript waits for the cleanup', async ({ page }) => {
  await open(page, { cleanup: 'on', hold: 'cleanup' });
  await press(page, 'New', 'Record', 'Done');
  await expect(status(page)).toHaveText('Transcribing…');
  expect(await copied(page)).toEqual([]);
  await release(page, 'cleanup');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is part 1 of the dictation.');
  await expect.poll(() => copied(page)).toEqual(['This is part 1 of the dictation.']);
});
