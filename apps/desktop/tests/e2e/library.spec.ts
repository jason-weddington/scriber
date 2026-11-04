import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, copied, expectEveryItemReachable, hold, nextFrame, open, press, release, status } from './helpers';

// The recording library: the drawer on the left lists every finished
// dictation newest first, grouped by day, each row its summary on up to two
// lines with its start time (local) beneath. An excerpt stands in until the
// local model's summary replaces it. The
// mock archives like the core (src/lib/mock.ts) and summarizes every entry
// as "A dictated note." unless summaries are off or Ollama is down.

// The page's clock is fixed at 14:03:09 in the page's own time zone.
test.use({ timezoneId: 'UTC' });
const NOW = Date.UTC(2026, 9, 26, 14, 3, 9);
const TIME = '2:03 PM';
const EXCERPT = 'This is part 1 of the...';
const SUMMARY = 'A dictated note.';

test.beforeEach(async ({ page }) => {
  await page.clock.setFixedTime(NOW);
});

function rows(page: Page) {
  return page.locator('#library li');
}

function drawer(page: Page) {
  return page.getByRole('navigation', { name: 'Recordings' });
}

function summarySwitch(page: Page) {
  return page.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' });
}

/** Every settings_set call the mock received. */
async function saved(page: Page): Promise<unknown[]> {
  return page.evaluate(() => [...(window.__mockBridge?.saved ?? [])]);
}

async function dictate(page: Page): Promise<void> {
  await press(page, 'New', 'Record', 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/^This is part 1 of the dictation\./);
}

test('a finished session appears at the top with its excerpt, then its summary', async ({ page }) => {
  await open(page);
  await expect(drawer(page)).toContainText('No recordings yet.');
  await hold(page, 'summarize');
  await dictate(page);
  await expect(rows(page)).toHaveCount(1);
  await expect(rows(page).first().locator('.entry-time')).toHaveText(TIME);
  await expect(rows(page).first().locator('.entry-summary')).toHaveText(EXCERPT);
  await expect(drawer(page)).not.toContainText('No recordings yet.');
  await expect(page.getByText(/ollama|summar/i).filter({ visible: true })).toHaveCount(0);
  await release(page, 'summarize');
  await expect(rows(page).first().locator('.entry-summary')).toHaveText(SUMMARY);
  // No status about Ollama or the summary's progress, anywhere.
  await expect(page.getByText(/ollama|summar/i).filter({ visible: true })).toHaveCount(0);
});

for (const params of [{ summaries: 'off' }, { ollama: 'down' }]) {
  test(`with ${JSON.stringify(params)} the row keeps its excerpt`, async ({ page }) => {
    await open(page, params);
    await dictate(page);
    await expect(rows(page).first().locator('.entry-summary')).toHaveText(EXCERPT);
    // A second session archives after the first one's summary would have landed.
    await dictate(page);
    await expect(rows(page)).toHaveCount(2);
    await expect(rows(page).locator('.entry-summary')).toHaveText([EXCERPT, EXCERPT]);
  });
}

test('turning summaries off in the settings keeps the next excerpt', async ({ page }) => {
  await open(page);
  await expect(summarySwitch(page)).toBeHidden();
  await button(page, 'Settings').click();
  await expect(button(page, 'Settings')).toHaveAttribute('aria-haspopup', 'dialog');
  await expect(button(page, 'Settings')).not.toHaveAttribute('aria-expanded', /.*/);
  await expect(button(page, 'Settings')).not.toHaveAttribute('aria-controls', /.*/);
  await expect(summarySwitch(page)).toBeChecked();
  await summarySwitch(page).uncheck();
  await expect.poll(() => saved(page)).toEqual([{ summaries: false, libraryOpen: true, vocabulary: '/mock/data/vocabulary.txt', cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: '/mock/data/learned.txt', modelFolder: '/mock/data/models' }]);
  await page.keyboard.press('Escape');
  await expect(summarySwitch(page)).toBeHidden();
  await dictate(page);
  await expect(rows(page).first().locator('.entry-summary')).toHaveText(EXCERPT);
  await dictate(page);
  await expect(rows(page).locator('.entry-summary')).toHaveText([EXCERPT, EXCERPT]);
  await button(page, 'Settings').click();
  await expect(summarySwitch(page)).not.toBeChecked();
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await expect(summarySwitch(page)).toBeHidden();
});

test('a leftover session finished by Retry is titled by the time it finished', async ({ page }) => {
  await open(page, { leftover: '2' });
  await press(page, 'Retry');
  await expect(rows(page)).toHaveCount(1);
  await expect(rows(page).first().locator('.entry-time')).toHaveText(TIME);
});

test('selecting an entry shows its transcript read-only, Copy copies it, New goes back', async ({ page }) => {
  await open(page, { library: '3' });
  await expect(rows(page)).toHaveCount(3);
  await expect(rows(page).locator('.entry-time')).toHaveText(['9:00 AM', '8:59 AM', '8:58 AM']);
  await rows(page).nth(1).getByRole('button').click();
  await expect(rows(page).nth(1).getByRole('button')).toHaveAttribute('aria-current', 'true');
  await expect(page.getByRole('heading', { level: 2 })).toHaveText('2026-10-26 08:59:00');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toHaveValue('This is preloaded recording number 2.');
  await expect(transcript).not.toBeEditable();
  await expect(status(page)).toHaveCount(0);
  await press(page, 'Copy');
  await expect(page.locator('.entry-status')).toHaveText('Copied to the clipboard.');
  expect(await copied(page)).toEqual(['This is preloaded recording number 2.']);
  await rows(page).first().getByRole('button').click();
  await expect(transcript).toHaveValue('This is preloaded recording number 1.');
  await expect(page.locator('.entry-status')).toHaveText('');
  await press(page, 'New');
  await expect(page.getByRole('heading', { level: 2 })).toHaveCount(0);
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await expect(rows(page).first().getByRole('button')).not.toHaveAttribute('aria-current');
});

test('New from an entry only goes back when the session cannot start anew', async ({ page }) => {
  await open(page, { library: '1' });
  await press(page, 'New', 'Record');
  await rows(page).first().getByRole('button').click();
  await expect(button(page, 'New')).toBeEnabled();
  await page.keyboard.press('Meta+n');
  await expect(page.getByRole('heading', { level: 2 })).toHaveCount(0);
  await expect(status(page)).toContainText('Recording.');
  expect(await page.evaluate(() => [...(window.__mockBridge?.acts ?? [])])).toEqual(['new', 'record']);
});

test('the Library button hides and shows the drawer and saves the choice', async ({ page }) => {
  await open(page, { library: '2' });
  const toggle = button(page, 'Library');
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await expect(toggle).toHaveAttribute('aria-controls', 'library');
  await expect(drawer(page)).toBeVisible();
  await toggle.click();
  await expect(drawer(page)).toBeHidden();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await toggle.click();
  await expect(drawer(page)).toBeVisible();
  await expect
    .poll(() => saved(page))
    .toEqual([
      { summaries: true, libraryOpen: false, vocabulary: '/mock/data/vocabulary.txt', cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: '/mock/data/learned.txt', modelFolder: '/mock/data/models' },
      { summaries: true, libraryOpen: true, vocabulary: '/mock/data/vocabulary.txt', cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: '/mock/data/learned.txt', modelFolder: '/mock/data/models' },
    ]);
});

test('the drawer starts closed when the settings say so', async ({ page }) => {
  await open(page, { libraryOpen: '0' });
  await expect(button(page, 'Library')).toHaveAttribute('aria-expanded', 'false');
  await expect(drawer(page)).toBeHidden();
});

test('a long library scrolls inside the drawer at the minimum window size', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await open(page, { library: '30' });
  await expect(rows(page)).toHaveCount(30);
  await expectEveryItemReachable(rows(page));
  const fits = await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight);
  expect(fits, 'the window itself never scrolls').toBe(true);
});

test('a long library scrolls inside the narrowest drawer too', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await open(page, { library: '30', libraryWidth: '110' });
  await expect(rows(page)).toHaveCount(30);
  await expectEveryItemReachable(rows(page));
  const fits = await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight);
  expect(fits, 'the window itself never scrolls').toBe(true);
});

test('a library that cannot be read says why in the drawer', async ({ page }) => {
  await open(page, { fail: 'library_list' });
  await expect(drawer(page).getByRole('alert')).toHaveText('Could not read the library: Mock failure: library_list');
});

test('an entry that cannot be read says why in the pane', async ({ page }) => {
  await open(page, { fail: 'library_get', library: '1' });
  await rows(page).first().getByRole('button').click();
  await expect(page.getByRole('main').getByRole('alert')).toHaveText('Mock failure: library_get');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveCount(0);
});

test('a copy that fails says why in the entry view', async ({ page }) => {
  await open(page, { fail: 'library_copy', library: '1' });
  await rows(page).first().getByRole('button').click();
  await press(page, 'Copy');
  await expect(page.getByRole('main').getByRole('alert')).toHaveText('Mock failure: library_copy');
  await expect(page.locator('.entry-status')).toHaveText('');
});

test('settings that cannot be saved say why, and the drawer still flips', async ({ page }) => {
  await open(page, { fail: 'settings_set' });
  await button(page, 'Settings').click();
  await summarySwitch(page).uncheck();
  await expect(page.getByRole('dialog', { name: 'Settings' }).getByRole('alert')).toHaveText('Mock failure: settings_set');
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await button(page, 'Library').click();
  await expect(drawer(page)).toBeHidden();
  await expect(page.getByRole('banner').getByRole('alert')).toHaveText('Mock failure: settings_set');
});

test('settings that cannot be read leave the defaults, silently', async ({ page }) => {
  await open(page, { fail: 'settings_get' });
  await expect(drawer(page)).toBeVisible();
  await button(page, 'Settings').click();
  await expect(summarySwitch(page)).toBeChecked();
  await expect(page.getByRole('textbox', { name: 'Vocabulary file', exact: true })).toHaveValue('');
  await expect(page.getByRole('alert')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await button(page, 'Library').click();
  await expect.poll(() => saved(page)).toEqual([{ summaries: true, libraryOpen: false, vocabulary: '', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: '', modelFolder: '' }]);
});

test('the check resets when another entry is shown', async ({ page }) => {
  await open(page, { library: '2' });
  const rows = page.locator('#library li');
  await rows.nth(0).getByRole('button').click();
  await press(page, 'Copy');
  await expect(page.locator('button.copy.copied')).toBeVisible();
  await rows.nth(1).getByRole('button').click();
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is preloaded recording number 2.');
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});

test('a failed entry copy shows no check', async ({ page }) => {
  await open(page, { fail: 'library_copy', library: '1' });
  await page.locator('#library li').nth(0).getByRole('button').click();
  await press(page, 'Copy');
  await expect(page.getByRole('main').getByRole('alert')).toHaveText('Mock failure: library_copy');
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});
