import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, copied, nextFrame, open, press, release, status } from './helpers';

// The settings panel's vocabulary file: its file name (the default one in the
// data folder until another is picked), Browse… (the native open panel,
// answered by the mock's ?pick=) and Open in Text Editor; then the copy-sound
// switch, which plays Pop when the transcript lands on the clipboard; then the
// post-processing switch, which cleans up each transcript with the local model;
// and the two model dropdowns, filled from the installed Ollama models each
// time the panel opens.

const DEFAULT = '/mock/data/vocabulary.txt';
const PICKED = '/mock/Documents/terms.txt';
const DEFAULT_NAME = 'vocabulary.txt';
const PICKED_NAME = 'terms.txt';
const DEFAULT_LEARNED = '/mock/data/learned.txt';
const DEFAULT_LEARNED_NAME = 'learned.txt';
const DEFAULT_MODEL = '/mock/data/models';
const DEFAULT_MODEL_NAME = 'models';
const PICKED_MODEL = '/mock/Models';

function vocabularyGroup(page: Page) {
  return page.getByRole('group', { name: 'Vocabulary file', exact: true });
}

function path(page: Page) {
  return vocabularyGroup(page).getByRole('textbox', { name: 'Vocabulary file', exact: true });
}

function browse(page: Page) {
  return vocabularyGroup(page).getByRole('button', { name: 'Browse…' });
}

function editor(page: Page) {
  return vocabularyGroup(page).getByRole('button', { name: 'Open in Text Editor' });
}

function learnedGroup(page: Page) {
  return page.getByRole('group', { name: 'Learned vocabulary file', exact: true });
}

function learnedPath(page: Page) {
  return learnedGroup(page).getByRole('textbox', { name: 'Learned vocabulary file', exact: true });
}

function learnedBrowse(page: Page) {
  return learnedGroup(page).getByRole('button', { name: 'Browse…' });
}

function learnedEditor(page: Page) {
  return learnedGroup(page).getByRole('button', { name: 'Open in Text Editor' });
}

function modelGroup(page: Page) {
  return page.getByRole('group', { name: 'Speech model folder', exact: true });
}

function modelPath(page: Page) {
  return modelGroup(page).getByRole('textbox', { name: 'Speech model folder', exact: true });
}

function modelBrowse(page: Page) {
  return modelGroup(page).getByRole('button', { name: 'Browse…' });
}

function alert(page: Page) {
  return page.getByRole('dialog', { name: 'Settings' }).getByRole('alert');
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

test("the vocabulary file shows the default file's name at launch", async ({ page }) => {
  await openSettings(page);
  await expect(path(page)).toHaveValue(DEFAULT_NAME);
  await expect(path(page)).toHaveAttribute('readonly', '');
  await expect(path(page)).toHaveAttribute('title', DEFAULT);
  await expect(path(page)).toHaveAccessibleDescription(DEFAULT);
  await expect(vocabularyGroup(page)).toBeVisible();
});

test("the learned vocabulary file shows the default file's name at launch", async ({ page }) => {
  await openSettings(page);
  await expect(learnedPath(page)).toHaveValue(DEFAULT_LEARNED_NAME);
  await expect(learnedPath(page)).toHaveAttribute('readonly', '');
  await expect(learnedPath(page)).toHaveAttribute('title', DEFAULT_LEARNED);
  await expect(learnedPath(page)).toHaveAccessibleDescription(DEFAULT_LEARNED);
  await expect(learnedGroup(page)).toBeVisible();
});

test('Browse picks another file and saves it', async ({ page }) => {
  await openSettings(page, { pick: PICKED });
  await browse(page).click();
  await expect(path(page)).toHaveValue(PICKED_NAME);
  await expect(path(page)).toHaveAttribute('title', PICKED);
  expect(await page.evaluate(() => [...(window.__mockBridge?.browsed ?? [])]), 'the panel starts at the current file').toEqual([DEFAULT]);
  await expect.poll(async () => (await saved(page)).at(-1)).toEqual({ summaries: true, libraryOpen: true, vocabulary: PICKED, cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
  await expect(alert(page)).toHaveCount(0);
});

test('a cancelled Browse changes nothing', async ({ page }) => {
  await openSettings(page, { pick: 'cancel' });
  await browse(page).click();
  await editor(page).click();
  await expect.poll(() => opened(page)).toEqual([DEFAULT]);
  await expect(path(page)).toHaveValue(DEFAULT_NAME);
  expect(await saved(page)).toEqual([]);
});

test('an open panel that fails says why and saves nothing', async ({ page }) => {
  await openSettings(page, { fail: 'pick_vocabulary_file' });
  await browse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: pick_vocabulary_file');
  await expect(path(page)).toHaveValue(DEFAULT_NAME);
  expect(await saved(page)).toEqual([]);
});

test('a picked file that cannot be saved stays on screen and says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set', pick: PICKED });
  await browse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(path(page)).toHaveValue(PICKED_NAME);
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
  await expect(alert(page)).toHaveCSS('font-size', '12px');
  expect(await opened(page)).toEqual([]);
});

test('Browse picks another learned vocabulary file and saves it', async ({ page }) => {
  await openSettings(page, { pick: PICKED });
  await learnedBrowse(page).click();
  await expect(learnedPath(page)).toHaveValue(PICKED_NAME);
  await expect(learnedPath(page)).toHaveAttribute('title', PICKED);
  expect(await page.evaluate(() => [...(window.__mockBridge?.browsed ?? [])]), 'the panel starts at the current learned file').toEqual([DEFAULT_LEARNED]);
  await expect.poll(async () => (await saved(page)).at(-1)).toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: PICKED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
  await expect(alert(page)).toHaveCount(0);
});

test('Open in Text Editor opens the learned vocabulary file', async ({ page }) => {
  await openSettings(page);
  await learnedEditor(page).click();
  await expect.poll(() => opened(page)).toEqual([DEFAULT_LEARNED]);
  await expect(alert(page)).toHaveCount(0);
});

test('a text editor that fails on the learned vocabulary file says why', async ({ page }) => {
  await openSettings(page, { fail: 'learned_vocabulary_open' });
  await learnedEditor(page).click();
  await expect(alert(page)).toHaveText('Could not open the vocabulary file: no text editor.');
  expect(await opened(page)).toEqual([]);
});

test('an open panel that fails for the learned vocabulary file says why and saves nothing', async ({ page }) => {
  await openSettings(page, { fail: 'pick_vocabulary_file' });
  await learnedBrowse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: pick_vocabulary_file');
  await expect(learnedPath(page)).toHaveValue(DEFAULT_LEARNED_NAME);
  expect(await saved(page)).toEqual([]);
});

test('a picked learned file that cannot be saved stays on screen and says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set', pick: PICKED });
  await learnedBrowse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(learnedPath(page)).toHaveValue(PICKED_NAME);
});

test("the speech model folder shows the default folder's name at launch", async ({ page }) => {
  await openSettings(page);
  await expect(modelPath(page)).toHaveValue(DEFAULT_MODEL_NAME);
  await expect(modelPath(page)).toHaveAttribute('readonly', '');
  await expect(modelPath(page)).toHaveAttribute('title', DEFAULT_MODEL);
  await expect(modelPath(page)).toHaveAccessibleDescription(DEFAULT_MODEL);
  await expect(modelGroup(page)).toBeVisible();
});

test('Browse picks another model folder and saves it', async ({ page }) => {
  await openSettings(page, { picked: PICKED_MODEL });
  await modelBrowse(page).click();
  await expect(modelPath(page)).toHaveValue('Models');
  await expect(modelPath(page)).toHaveAttribute('title', PICKED_MODEL);
  expect(await page.evaluate(() => [...(window.__mockBridge?.browsed ?? [])]), 'the panel starts at the current folder').toEqual([DEFAULT_MODEL]);
  await expect.poll(async () => (await saved(page)).at(-1)).toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: PICKED_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
  await expect(alert(page)).toHaveCount(0);
});

test('a cancelled model folder Browse changes nothing', async ({ page }) => {
  await openSettings(page, { picked: 'cancel' });
  await modelBrowse(page).click();
  await expect(modelPath(page)).toHaveValue(DEFAULT_MODEL_NAME);
  expect(await saved(page)).toEqual([]);
});

test('a model folder panel that fails says why and saves nothing', async ({ page }) => {
  await openSettings(page, { fail: 'pick_model_folder' });
  await modelBrowse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: pick_model_folder');
  await expect(modelPath(page)).toHaveValue(DEFAULT_MODEL_NAME);
  expect(await saved(page)).toEqual([]);
});

test('a picked model folder that cannot be saved stays on screen and says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set', picked: PICKED_MODEL });
  await modelBrowse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(modelPath(page)).toHaveValue('Models');
});

test('a model folder saved when the session cannot be re-read says why', async ({ page }) => {
  await openSettings(page, { fail: 'dictation_state', picked: PICKED_MODEL });
  await modelBrowse(page).click();
  await expect(alert(page)).toHaveText('Mock failure: dictation_state');
  await expect(modelPath(page)).toHaveValue('Models');
});

test('a long path never scrolls the window sideways at the minimum size', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  const long = `/${'a'.repeat(195)}.txt`;
  await openSettings(page, { pick: long });
  await browse(page).click();
  await expect(path(page)).toHaveValue(long.slice(1));
  await expect(editor(page)).toBeInViewport();
  await learnedBrowse(page).click();
  await expect(learnedPath(page)).toHaveValue(long.slice(1));
  await expect(learnedEditor(page)).toBeInViewport();
  const fits = await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);
  expect(fits, 'no horizontal scroll').toBe(true);
});

function cleanupSwitch(page: Page) {
  return page.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' });
}

test('post-processing starts off and turning it on saves it', async ({ page }) => {
  await openSettings(page);
  await expect(cleanupSwitch(page)).not.toBeChecked();
  await cleanupSwitch(page).click();
  await expect(cleanupSwitch(page)).toBeChecked();
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: true, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
  await expect(alert(page)).toHaveCount(0);
});

test('a post-processing switch that cannot be saved says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await cleanupSwitch(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
});

function copySoundSwitch(page: Page) {
  return page.getByRole('switch', { name: 'Play a sound when the transcript is copied' });
}

test('the copy sound starts on and turning it off saves it', async ({ page }) => {
  await openSettings(page);
  await expect(copySoundSwitch(page)).toBeChecked();
  await copySoundSwitch(page).click();
  await expect(copySoundSwitch(page)).not.toBeChecked();
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'qwen3.8:27b-mlx', cleanupModel: 'qwen3.8:27b-mlx', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: false });
  await expect(alert(page)).toHaveCount(0);
});

test('a copy-sound switch that cannot be saved says why', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await copySoundSwitch(page).click();
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
});

test('?copySound=off starts the copy-sound switch unchecked', async ({ page }) => {
  await openSettings(page, { copySound: 'off' });
  await expect(copySoundSwitch(page)).not.toBeChecked();
});

test('with post-processing on, the transcript waits for the cleanup', async ({ page }) => {
  await open(page, { cleanup: 'on', hold: 'cleanup' });
  await press(page, 'New', 'Record', 'Done');
  await expect(status(page)).toHaveText('Post-processing with local AI…');
  expect(await copied(page)).toEqual([]);
  await release(page, 'cleanup');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is part 1 of the dictation.');
  await expect.poll(() => copied(page)).toEqual(['This is part 1 of the dictation.']);
});

const MODEL = 'qwen3.8:27b-mlx';
const MODELS = ['gemma3:4b', MODEL];
const DOWN_NOTE = 'Local AI needs Ollama. Start Ollama, then reopen Settings.';

function summaryModel(page: Page) {
  return page.getByRole('combobox', { name: 'Title model' });
}

function cleanupModel(page: Page) {
  return page.getByRole('combobox', { name: 'Post-processing model' });
}

function summarySwitch(page: Page) {
  return page.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' });
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
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'gemma3:4b', cleanupModel: MODEL, libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
  await cleanupModel(page).selectOption('gemma3:4b');
  await expect
    .poll(async () => (await saved(page)).at(-1))
    .toEqual({ summaries: true, libraryOpen: true, vocabulary: DEFAULT, cleanup: false, summaryModel: 'gemma3:4b', cleanupModel: 'gemma3:4b', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED, modelFolder: DEFAULT_MODEL, toggleShortcut: 'Opt+Cmd+D', pauseShortcut: 'Opt+Cmd+P', copySound: true });
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

test('the models are listed each time the modal opens, and only then', async ({ page }) => {
  await open(page);
  expect(await listings(page)).toBe(0);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeEnabled();
  expect(await listings(page)).toBe(1);
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await expect(summaryModel(page)).toBeHidden();
  expect(await listings(page)).toBe(1);
  await button(page, 'Settings').click();
  await expect(summaryModel(page)).toBeVisible();
  expect(await listings(page)).toBe(2);
  await page.keyboard.press('Escape');
  await nextFrame(page);
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
  for (const target of [note, summaryModel(page), page.getByText('Version 0.0.0-mock')]) {
    await target.scrollIntoViewIfNeeded();
    await expect(target).toBeInViewport();
  }
});

test('a stale listing is dropped and the latest one wins', async ({ page }) => {
  await openSettings(page, { hold: 'ollama_models' });
  await page.keyboard.press('Escape');
  await nextFrame(page);
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
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await button(page, 'Settings').click();
  await release(page, 'ollama_models');
  await expect(page.getByText(DOWN_NOTE)).toBeVisible();
  expect(await listings(page)).toBe(2);
  await expect(summaryModel(page)).toBeDisabled();
});

// The modal itself: a native dialog, centred over a dimming overlay.

function modal(page: Page) {
  return page.getByRole('dialog', { name: 'Settings' });
}

test('Settings is a native modal dialog over a dimming overlay', async ({ page }) => {
  await open(page);
  await expect(page.getByRole('dialog', { name: 'Settings' })).toHaveCount(0);
  await button(page, 'Settings').click();
  await expect(modal(page)).toBeVisible();
  expect(await modal(page).evaluate((d) => d.matches(':modal'))).toBe(true);
  expect(await modal(page).evaluate((d) => getComputedStyle(d, '::backdrop').backgroundColor)).toBe('rgba(0, 0, 0, 0.45)');
  await expect(page.getByRole('banner').getByRole('dialog')).toHaveCount(0);
  await expect(modal(page).getByRole('heading', { name: 'Settings' })).toBeVisible();
});

test('the modal is centred in the window', async ({ page }) => {
  await page.setViewportSize({ width: 960, height: 640 });
  await openSettings(page);
  const box = await modal(page).boundingBox();
  expect(box).not.toBeNull();
  expect(Math.abs((box?.x ?? 0) + (box?.width ?? 0) / 2 - 480)).toBeLessThanOrEqual(1);
  expect(Math.abs((box?.y ?? 0) + (box?.height ?? 0) / 2 - 320)).toBeLessThanOrEqual(1);
});

test('the whole modal shows in the narrowest window', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 380 });
  await openSettings(page);
  const box = await modal(page).boundingBox();
  expect(box?.width).toBe(488);
  expect(box?.x).toBeGreaterThanOrEqual(0);
  expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(520);
  expect(box?.y).toBeGreaterThanOrEqual(0);
  expect((box?.y ?? 0) + (box?.height ?? 0)).toBeLessThanOrEqual(380);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  for (const target of [path(page), browse(page), editor(page), learnedPath(page), learnedBrowse(page), learnedEditor(page), cleanupSwitch(page), summarySwitch(page), summaryModel(page), cleanupModel(page)]) {
    await target.scrollIntoViewIfNeeded();
    await expect(target).toBeInViewport();
  }
});

test('a modal taller than the window scrolls inside itself', async ({ page }) => {
  await page.setViewportSize({ width: 520, height: 240 });
  await openSettings(page, { ollama: 'down' });
  const body = page.locator('.settings-body');
  expect(await body.evaluate((b) => b.scrollHeight > b.clientHeight)).toBe(true);
  const box = await modal(page).boundingBox();
  expect(box?.y).toBeGreaterThanOrEqual(0);
  expect((box?.y ?? 0) + (box?.height ?? 0)).toBeLessThanOrEqual(240);
  const note = page.getByText(DOWN_NOTE);
  await note.scrollIntoViewIfNeeded();
  await expect(note).toBeInViewport();
  expect(await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight)).toBe(true);
});

test('the vocabulary path has the focus on opening, by click or by Enter', async ({ page }) => {
  await open(page);
  await button(page, 'Settings').click();
  await expect(path(page)).toBeFocused();
  await page.keyboard.press('Escape');
  await nextFrame(page);
  await button(page, 'Settings').focus();
  await page.keyboard.press('Enter');
  await expect(path(page)).toBeFocused();
});

test('Tab stays inside the modal', async ({ page }) => {
  await openSettings(page);
  await expect(summaryModel(page)).toBeEnabled();
  for (let i = 0; i < 10; i++) {
    await page.keyboard.press('Tab');
    const inside = await page.evaluate(() => document.activeElement === document.body || (document.querySelector('#settings')?.contains(document.activeElement) ?? false));
    expect(inside).toBe(true);
  }
});

test('Escape closes the modal from the textbox and from a switch', async ({ page }) => {
  await openSettings(page);
  await page.keyboard.press('Escape');
  await expect(modal(page)).toBeHidden();
  await nextFrame(page);
  await button(page, 'Settings').click();
  await cleanupSwitch(page).focus();
  await page.keyboard.press('Escape');
  await expect(modal(page)).toBeHidden();
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
});

test('the Close button closes the modal and returns the focus to the gear', async ({ page }) => {
  await openSettings(page);
  await button(page, 'Close').click();
  await expect(modal(page)).toBeHidden();
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
});

test('a click on the overlay closes the modal; a click inside does not', async ({ page }) => {
  await openSettings(page);
  await vocabularyGroup(page).locator('.caption').click();
  await expect(modal(page)).toBeVisible();
  await page.mouse.click(5, 5);
  await expect(modal(page)).toBeHidden();
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
});

test('a drag from inside the modal released on the overlay leaves it open', async ({ page }) => {
  await openSettings(page);
  const box = await vocabularyGroup(page).locator('.caption').boundingBox();
  await page.mouse.move((box?.x ?? 0) + (box?.width ?? 0) / 2, (box?.y ?? 0) + (box?.height ?? 0) / 2);
  await page.mouse.down();
  await page.mouse.move(5, 5);
  await page.mouse.up();
  await nextFrame(page);
  await expect(modal(page)).toBeVisible();
  expect(await saved(page)).toEqual([]);
});

test('the version moved from the window into Settings', async ({ page }) => {
  await openSettings(page);
  await expect(page.getByRole('dialog', { name: 'Settings' }).getByText('Version 0.0.0-mock')).toBeVisible();
  await expect(page.getByRole('contentinfo')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await expect(page.getByText('Version 0.0.0-mock')).toBeHidden();
});

test('the version is the last thing in Settings, below the status row', async ({ page }) => {
  await openSettings(page, { ollama: 'down', fail: 'vocabulary_open' });
  await editor(page).click();
  await expect(alert(page)).toBeVisible();
  await expect(page.locator('.settings .note')).toHaveClass(/visually-hidden/);
  await expect(page.locator('.settings-body > .version:last-child')).toHaveText('Version 0.0.0-mock');
});

function splitSelect(page: Page) {
  return page.getByLabel('Split into parts at a pause');
}

test('the silence split is chosen in Settings and saved', async ({ page }) => {
  await openSettings(page);
  await expect(splitSelect(page).locator('option')).toHaveText(['Off', '1.5 s', '2.5 s', '4 s']);
  await expect(splitSelect(page)).toHaveValue('0');
  await splitSelect(page).selectOption({ label: '2.5 s' });
  await expect.poll(async () => ((await saved(page)).at(-1) as { silenceSplitMs?: number } | undefined)?.silenceSplitMs).toBe(2500);
  await expect(splitSelect(page)).toHaveValue('2500');
});

test('a saved silence split outside the four is shown first, selected', async ({ page }) => {
  await openSettings(page, { silenceSplitMs: '3000' });
  await expect(splitSelect(page).locator('option')).toHaveText(['3 s', 'Off', '1.5 s', '2.5 s', '4 s']);
  await expect(splitSelect(page)).toHaveValue('3000');
  await expect(splitSelect(page)).toBeEnabled();
});

test('a silence split that fails to save says so', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await splitSelect(page).selectOption({ label: '4 s' });
  await expect(alert(page)).toContainText('Mock failure: settings_set');
});

function shortcutButton(page: Page) {
  return page.getByRole('button', { name: 'Toggle dictation shortcut' });
}

test('the toggle shortcut shows ⌥⌘D at launch and arms a capture', async ({ page }) => {
  await openSettings(page);
  const button = shortcutButton(page);
  await expect(button).toBeVisible();
  await expect(button).toHaveText('⌥⌘D');
  await expect(button).toHaveAttribute('aria-keyshortcuts', 'Alt+Meta+D');
  await expect(button).toHaveAttribute('title', 'Alt+Meta+D');
  await button.click();
  await expect(button).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('a captured shortcut is saved and shown', async ({ page }) => {
  await openSettings(page);
  await shortcutButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyK');
  await expect(shortcutButton(page)).toHaveText('⌥⌘K');
  await expect(shortcutButton(page)).toHaveAttribute('aria-keyshortcuts', 'Alt+Meta+K');
  await expect.poll(async () => (await saved(page)).at(-1)).toMatchObject({ toggleShortcut: 'Opt+Cmd+K' });
  await expect(alert(page)).toHaveCount(0);
});

test('Escape cancels a capture, leaves the dialog open and saves nothing', async ({ page }) => {
  await openSettings(page);
  await shortcutButton(page).click();
  await expect(shortcutButton(page)).toHaveText('Press a shortcut…');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  await expect(shortcutButton(page)).toHaveText('⌥⌘D');
  expect(await saved(page)).toEqual([]);
  await expect(alert(page)).toHaveCount(0);
});

test('a key without ⌥ or ⌘ is refused and the capture stays armed', async ({ page }) => {
  await openSettings(page);
  await shortcutButton(page).click();
  await page.keyboard.press('d');
  await expect(alert(page)).toHaveText('A shortcut needs ⌥ or ⌘.');
  await expect(shortcutButton(page)).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('a key that cannot be a shortcut is refused', async ({ page }) => {
  await openSettings(page);
  await shortcutButton(page).click();
  await page.keyboard.press('Meta+Alt+Digit1');
  await expect(alert(page)).toHaveText('That key cannot be a shortcut.');
  await expect(shortcutButton(page)).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('a shortcut that cannot be saved says why and keeps the saved one', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await shortcutButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyK');
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(shortcutButton(page)).toHaveText('⌥⌘D');
});
function pauseButton(page: Page) {
  return page.getByRole('button', { name: 'Pause shortcut' });
}

test('the pause shortcut shows ⌥⌘P at launch and arms a capture', async ({ page }) => {
  await openSettings(page);
  const button = pauseButton(page);
  await expect(button).toBeVisible();
  await expect(button).toHaveText('⌥⌘P');
  await expect(button).toHaveAttribute('aria-keyshortcuts', 'Alt+Meta+P');
  await expect(button).toHaveAttribute('title', 'Alt+Meta+P');
  await expect(button).toHaveAccessibleName('Pause shortcut: ⌥⌘P');
  await button.click();
  await expect(button).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
  await expect(shortcutButton(page)).toHaveText('⌥⌘D');
});

test('a captured pause shortcut is saved and shown, leaving the toggle alone', async ({ page }) => {
  await openSettings(page);
  await pauseButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyJ');
  await expect(pauseButton(page)).toHaveText('⌥⌘J');
  await expect(pauseButton(page)).toHaveAttribute('aria-keyshortcuts', 'Alt+Meta+J');
  await expect.poll(async () => (await saved(page)).at(-1)).toMatchObject({ pauseShortcut: 'Opt+Cmd+J', toggleShortcut: 'Opt+Cmd+D' });
  await expect(alert(page)).toHaveCount(0);
});

test('Escape cancels a pause capture and saves nothing', async ({ page }) => {
  await openSettings(page);
  await pauseButton(page).click();
  await expect(pauseButton(page)).toHaveText('Press a shortcut…');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  await expect(pauseButton(page)).toHaveText('⌥⌘P');
  expect(await saved(page)).toEqual([]);
  await expect(alert(page)).toHaveCount(0);
});

test('the pause capture refuses the toggle shortcut', async ({ page }) => {
  await openSettings(page);
  await pauseButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyD');
  await expect(alert(page)).toHaveText('That shortcut is the toggle shortcut.');
  await expect(pauseButton(page)).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('the toggle capture refuses the pause shortcut', async ({ page }) => {
  await openSettings(page);
  await shortcutButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyP');
  await expect(alert(page)).toHaveText('That shortcut is the pause shortcut.');
  await expect(shortcutButton(page)).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('a pause key without ⌥ or ⌘, or one that cannot be a shortcut, is refused', async ({ page }) => {
  await openSettings(page);
  await pauseButton(page).click();
  await page.keyboard.press('d');
  await expect(alert(page)).toHaveText('A shortcut needs ⌥ or ⌘.');
  await expect(pauseButton(page)).toHaveText('Press a shortcut…');
  await page.keyboard.press('Meta+Alt+Digit1');
  await expect(alert(page)).toHaveText('That key cannot be a shortcut.');
  await expect(pauseButton(page)).toHaveText('Press a shortcut…');
  expect(await saved(page)).toEqual([]);
});

test('a pause shortcut that cannot be saved says why and keeps the saved one', async ({ page }) => {
  await openSettings(page, { fail: 'settings_set' });
  await pauseButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyJ');
  await expect(alert(page)).toHaveText('Mock failure: settings_set');
  await expect(pauseButton(page)).toHaveText('⌥⌘P');
});

test('a pause shortcut on the toggle chord shows None and omits both attributes', async ({ page }) => {
  await openSettings(page, { toggleShortcut: 'Opt+Cmd+P' });
  const button = pauseButton(page);
  await expect(button).toHaveText('None');
  await expect(button).not.toHaveAttribute('aria-keyshortcuts', /.*/);
  await expect(button).not.toHaveAttribute('title', /.*/);
  await expect(button).toHaveAccessibleName('Pause shortcut: None');
  await button.click();
  await page.keyboard.press('Meta+Alt+KeyJ');
  await expect.poll(async () => (await saved(page)).at(-1)).toMatchObject({ pauseShortcut: 'Opt+Cmd+J' });
});

test('capturing the toggle adopts the pause shortcut the engine answered', async ({ page }) => {
  await openSettings(page, { toggleShortcut: 'Opt+Cmd+P' });
  await shortcutButton(page).click();
  await page.keyboard.press('Meta+Alt+KeyD');
  await expect(shortcutButton(page)).toHaveText('⌥⌘D');
  await expect(pauseButton(page)).toHaveText('⌥⌘P');
  await expect(pauseButton(page)).toHaveAttribute('aria-keyshortcuts', 'Alt+Meta+P');
});
