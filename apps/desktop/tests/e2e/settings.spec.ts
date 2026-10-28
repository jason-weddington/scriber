import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, copied, open, press, release, status } from './helpers';

// The settings panel's vocabulary file: its path (the default one in the
// data folder until another is picked), Browse… (the native open panel,
// answered by the mock's ?pick=) and Open in Text Editor; then the
// post-processing switch, which cleans up each transcript with the local model;
// and the two model dropdowns, filled from the installed Ollama models each
// time the panel opens.

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
  await expect.poll(async () => (await saved(page)).at(-1)).toEqual({ summaries: true, libraryOpen: true, vocabulary: PICKED, cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx' });
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
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: true, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx' });
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

const MODEL = 'qwen3.8:27b-mlx';
const MODELS = ['gemma3:4b', MODEL];
const DOWN_NOTE = 'Local AI needs Ollama. Start Ollama, then reopen Settings.';

function summaryModel(page: Page) {
  return page.getByRole('combobox', { name: 'Summary model' });
}

function cleanupModel(page: Page) {
  return page.getByRole('combobox', { name: 'Post-processing model' });
}

function summarySwitch(page: Page) {
  return page.getByRole('switch', { name: 'Summarize recordings with local AI (Ollama)' });
}

async function optionTexts(select: ReturnType<typeof summaryModel>): Promise<string[]> {
  return select.locator('option').allTextContents();
}

async function listings(page: Page): Promise<number | undefined> {
  return page.evaluate(() => window.__mockBridge?.modelListings);
}

test('the model dropdowns list the installed models, with the default chosen', async ({ page }) => {
  await openSettings(page);
  for (const select of [summaryModel(page), cleanupModel(page)]) {
    await expect(select).toBeEnabled();
    await expect(select).toHaveValue(MODEL);
    await expect.poll(() => optionTexts(select)).toEqual(MODELS);
  }
});

test('choosing a model saves it', async ({ page }) => {
  await openSettings(page);
  await summaryModel(page).selectOption('gemma3:4b');
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'gemma3:4b', cleanupModel: MODEL });
  await cleanupModel(page).selectOption('gemma3:4b');
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'gemma3:4b', cleanupModel: 'gemma3:4b' });
  await expect(alert(page)).toHaveCount(0);
});

test('a saved model that is not installed is still shown, marked', async ({ page }) => {
  await openSettings(page, { summaryModel: 'llama3:70b' });
  await expect(summaryModel(page)).toBeEnabled();
  await expect(summaryModel(page)).toHaveValue('llama3:70b');
  await expect.poll(async () => (await optionTexts(summaryModel(page)))[0]).toBe('llama3:70b (not installed)');
  await expect.poll(() => optionTexts(summaryModel(page))).toEqual(['llama3:70b (not installed)', ...MODELS]);
});

test('while the models are listed the panel is open and the dropdowns wait', async ({ page }) => {
  await openSettings(page, { hold: 'ollama_models' });
  for (const select of [summaryModel(page), cleanupModel(page)]) {
    await expect(select).toBeDisabled();
    expect(await optionTexts(select)).toEqual([MODEL]);
  }
  await expect(summarySwitch(page)).toBeEnabled();
  await expect(cleanupSwitch(page)).toBeEnabled();
  await expect(page.getByText('not installed')).toHaveCount(0);
  await expect(page.getByText('Local AI needs Ollama')).toHaveCount(0);
  await release(page, 'ollama_models');
  for (const select of [summaryModel(page), cleanupModel(page)]) {
    await expect(select).toBeEnabled();
    await expect.poll(() => optionTexts(select)).toEqual(MODELS);
  }
});

test('without Ollama the dropdowns are disabled, the switches are not, and one line says why', async ({ page }) => {
  await openSettings(page, { ollama: 'down' });
  const note = page.getByText(DOWN_NOTE);
  await expect(note).toBeVisible();
  await expect(note).toHaveAttribute('title', 'Local AI needs Ollama: connection refused.');
  for (const select of [summaryModel(page), cleanupModel(page)]) {
    await expect(select).toBeDisabled();
    expect(await optionTexts(select)).toEqual([MODEL]);
  }
  await expect(summarySwitch(page)).toBeEnabled();
  await expect(cleanupSwitch(page)).toBeEnabled();
  await expect(page.getByText('not installed')).toHaveCount(0);
  await summarySwitch(page).click();
  await expect(summarySwitch(page)).not.toBeChecked();
  await expect.poll(async () => ((await saved(page)).at(-1) as { summaries?: boolean } | undefined)?.summaries).toBe(false);
});

test('the models are listed each time the panel opens, and only then', async ({ page }) => {
  await open(page);
  expect(await listings(page)).toBe(0);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeEnabled();
  expect(await listings(page)).toBe(1);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeHidden();
  expect(await listings(page)).toBe(1);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeVisible();
  expect(await listings(page)).toBe(2);
  await page.keyboard.press('Escape');
  await expect(summaryModel(page)).toBeHidden();
  expect(await listings(page)).toBe(2);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeVisible();
  expect(await listings(page)).toBe(3);
});

test('a model that cannot be saved says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await expect(summaryModel(page)).toBeEnabled();
  await summaryModel(page).selectOption('gemma3:4b');
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
});

test('a post-processing model that cannot be saved says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await expect(cleanupModel(page)).toBeEnabled();
  await cleanupModel(page).selectOption('gemma3:4b');
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
});

test('without Ollama the panel fits the minimum size', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await openSettings(page, { ollama: 'down' });
  const note = page.getByText(DOWN_NOTE);
  await expect(note).toBeVisible();
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);
  expect(fits, 'no horizontal scroll').toBe(true);
  for (const target of [note, summaryModel(page)]) {
    await target.scrollIntoViewIfNeeded();
    await expect(target).toBeInViewport();
  }
});

test('a stale listing is dropped and the latest one wins', async ({ page }) => {
  await openSettings(page, { hold: 'ollama_models' });
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeHidden();
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeVisible();
  await expect(summaryModel(page)).toBeDisabled();
  await release(page, 'ollama_models');
  expect(await listings(page)).toBe(2);
  for (const select of [summaryModel(page), cleanupModel(page)]) {
    await expect(select).toBeEnabled();
    await expect.poll(() => optionTexts(select)).toEqual(MODELS);
  }
});

test('a stale failed listing is dropped too', async ({ page }) => {
  await openSettings(page, { hold: 'ollama_models', ollama: 'down' });
  await button(page, 'Settings').click();
  await button(page, 'Settings').click();
  await release(page, 'ollama_models');
  await expect(page.getByText(DOWN_NOTE)).toBeVisible();
  expect(await listings(page)).toBe(2);
  await expect(summaryModel(page)).toBeDisabled();
});
