import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, hold, open, press, release, status } from './helpers';

// The in-window shortcuts: ⌘N is New, Space is Record, Pause and Resume,
// Return is Done. Each does nothing where its action is not allowed, Space
// and Return type in the transcript, and a focused button never fires twice.

/** Every action the window sent, in order (the mock logs them). */
async function acts(page: Page): Promise<string[]> {
  return page.evaluate(() => [...(window.__mockBridge?.acts ?? [])]);
}

/** Presses `key` on the page and waits for the window to settle. */
async function key(page: Page, name: string): Promise<void> {
  await page.keyboard.press(name);
}

test('the whole flow from the keyboard: ⌘N, Space, Space, Space, Return', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Paused');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Enter');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  expect(await acts(page)).toEqual(['new', 'record', 'pause', 'resume', 'done']);
});

test('Return finishes from paused too', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Pause');
  await expect(status(page)).toContainText('Paused');
  await key(page, 'Enter');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  expect((await acts(page)).at(-1)).toBe('done');
});

test('a key whose action the phase does not allow sends nothing', async ({ page }) => {
  await open(page);
  await key(page, 'Space');
  await key(page, 'Enter');
  expect(await acts(page), 'idle: only ⌘N works').toEqual([]);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await key(page, 'Enter');
  await key(page, 'Meta+n');
  expect(await acts(page), 'ready: Return and ⌘N are not allowed').toEqual(['new']);
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Meta+n');
  expect(await acts(page), 'recording: ⌘N is not allowed').toEqual(['new', 'record']);
});

test('nothing is sent while an action is in flight', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await hold(page, 'dictation_act');
  await key(page, 'Space');
  await key(page, 'Space');
  await release(page, 'dictation_act');
  await expect(status(page)).toContainText('Recording.');
  expect(await acts(page)).toEqual(['new', 'record']);
});

test('Space and Return type in the transcript, and ⌘N still starts a new session from there', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await transcript.fill('One');
  await transcript.press('Space');
  await transcript.press('Enter');
  await transcript.press('t');
  await expect(transcript).toHaveValue('One \nt');
  expect(await acts(page)).toEqual(['new', 'record', 'done']);
  await transcript.press('Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect((await acts(page)).at(-1)).toBe('new');
});

test('a button that has focus from a click does not also fire on Space', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Discard');
  await expect.poll(() => acts(page)).toEqual(['new', 'discard']);
  await expect(button(page, 'Discard')).toBeEnabled();
  await expect(button(page, 'Discard')).toBeFocused();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  expect(await acts(page), 'Record, once, and no second Discard').toEqual(['new', 'discard', 'record']);
});

test('a button disabled by its own action gives the focus back, so the keys keep working', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await expect(button(page, 'New')).not.toBeFocused();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
});

test('a button reached with the keyboard keeps Space and Return, and fires once', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await button(page, 'Discard').focus();
  await expect(button(page, 'Discard')).toBeFocused();
  await key(page, 'Enter');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect(await acts(page), 'Discard, not Done').toEqual(['new', 'record', 'discard']);
});

test('a held Space does not toggle on its repeats', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await page.keyboard.down('Space');
  await expect(status(page)).toContainText('Recording.');
  await page.evaluate(() => {
    for (let i = 0; i < 3; i++) window.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', repeat: true }));
  });
  await page.keyboard.up('Space');
  await expect(status(page)).toContainText('Recording.');
  expect(await acts(page)).toEqual(['new', 'record']);
});

test('Record waits for the speech model even from the keyboard', async ({ page }) => {
  await open(page, { model: 'download', hold: 'download' });
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await key(page, 'Space');
  expect(await acts(page)).toEqual(['new']);
  await release(page, 'download');
});

test('each button shows its shortcut and names it for assistive tech', async ({ page }) => {
  await open(page);
  const want: [string, string, string][] = [
    ['New', '⌘N', 'Meta+N'],
    ['Record', 'Space', 'Space'],
    ['Pause', 'Space', 'Space'],
    ['Resume', 'Space', 'Space'],
    ['Done', '⏎', 'Enter'],
  ];
  for (const [name, hint, aria] of want) {
    await expect(button(page, name), name).toHaveAttribute('aria-keyshortcuts', aria);
    await expect(button(page, name).locator('kbd'), name).toHaveText(hint);
  }
  await expect(button(page, 'Discard')).not.toHaveAttribute('aria-keyshortcuts');
});

test('Tab to a button, and Return clicks it rather than finishing', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  // Pause, Done, Discard are the enabled buttons, in that order.
  for (let i = 0; i < 3; i++) await key(page, 'Tab');
  await expect(button(page, 'Discard')).toBeFocused();
  await key(page, 'Enter');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect(await acts(page)).toEqual(['new', 'record', 'discard']);
});

test('keys before the session has loaded do nothing', async ({ page }) => {
  await open(page, { hold: 'dictation_state' });
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await release(page, 'dictation_state');
  await expect(status(page)).toHaveText('Click New to start a dictation.');
  expect(await acts(page)).toEqual([]);
});

test('Space on the focused summaries switch toggles it and sends no action', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').click();
  const toggle = page.getByRole('switch', { name: 'Summarize recordings with local AI (Ollama)' });
  await toggle.focus();
  await key(page, 'Space');
  await expect(toggle).not.toBeChecked();
  expect(await acts(page)).toEqual(['new']);
});

test('Space on the focused post-processing switch toggles it and sends no action', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').click();
  const toggle = page.getByRole('switch', { name: 'Enable post-processing with local LLM' });
  await toggle.focus();
  await key(page, 'Space');
  await expect(toggle).toBeChecked();
  expect(await acts(page)).toEqual(['new']);
});

test('Space and Return while a library entry is shown send no action', async ({ page }) => {
  await open(page, { library: '1' });
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await page.locator('#library li').first().getByRole('button').click();
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is preloaded recording number 1.');
  await page.locator('body').click({ position: { x: 300, y: 200 } });
  await key(page, 'Space');
  await key(page, 'Enter');
  expect(await acts(page)).toEqual(['new']);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect(await acts(page), 'ready: ⌘N only goes back').toEqual(['new']);
});
