import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, hold, nextFrame, open, press, release, status } from './helpers';

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
  const check = async (name: string, hint: string, aria: string): Promise<void> => {
    await expect(button(page, name), name).toHaveAttribute('aria-keyshortcuts', aria);
    await expect(button(page, name).locator('kbd'), name).toHaveText(hint);
  };
  await open(page);
  await check('New', '⌘N', 'Meta+N');
  await press(page, 'New');
  await check('Record', 'Space', 'Space');
  await expect(button(page, 'Discard')).toBeVisible();
  await expect(button(page, 'Discard')).not.toHaveAttribute('aria-keyshortcuts');
  await expect(button(page, 'Discard').locator('kbd')).toHaveCount(0);
  await press(page, 'Record');
  await check('Pause', 'Space', 'Space');
  await check('Done', '⏎', 'Enter');
  await press(page, 'Pause');
  await check('Resume', 'Space', 'Space');
});

test('Tab to a button, and Return clicks it rather than finishing', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  // The microphone, Pause, Done, Discard are the buttons offered, in that order.
  await key(page, 'Tab');
  await expect(page.getByRole('button', { name: /^Microphone/ })).toBeFocused();
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
  const toggle = page.getByRole('switch', { name: 'Generate library titles with local AI (Ollama)' });
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
  const toggle = page.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' });
  await toggle.focus();
  await key(page, 'Space');
  await expect(toggle).toBeChecked();
  expect(await acts(page)).toEqual(['new']);
});

test('Space on the focused summary model dropdown sends no action', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').click();
  const dropdown = page.getByRole('combobox', { name: 'Title model' });
  await expect(dropdown).toBeEnabled();
  await dropdown.focus();
  await key(page, 'Space');
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

// Settings is modal: no shortcut acts behind it, and all of them come back.

test('Space does nothing behind the open Settings (ready)', async ({ page }) => {
  await open(page, { pick: 'cancel' });
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').click();
  await page.getByRole('button', { name: 'Browse…' }).click();
  await key(page, 'Space');
  expect(await acts(page)).toEqual(['new']);
});

test('Space and Return do nothing behind the open Settings (recording)', async ({ page }) => {
  await open(page, { pick: 'cancel' });
  await press(page, 'New', 'Record');
  await expect(status(page)).toContainText('Recording.');
  await button(page, 'Settings').click();
  await page.getByRole('button', { name: 'Browse…' }).click();
  await key(page, 'Space');
  await key(page, 'Enter');
  expect(await acts(page)).toEqual(['new', 'record']);
  await expect(status(page)).toContainText('Recording.');
});

test('⌘N does nothing behind the open Settings (idle)', async ({ page }) => {
  await open(page);
  await button(page, 'Settings').click();
  await key(page, 'Meta+n');
  expect(await acts(page)).toEqual([]);
  await expect(status(page)).not.toHaveText('Click Record and start talking.');
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
});

test('⌘N does nothing behind the open Settings (entry shown)', async ({ page }) => {
  await open(page, { library: '1' });
  await page.locator('#library li').first().getByRole('button').click();
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toHaveValue('This is preloaded recording number 1.');
  await button(page, 'Settings').click();
  await key(page, 'Meta+n');
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(transcript).toHaveValue('This is preloaded recording number 1.');
});

test('⌘N works again after Settings closes', async ({ page }) => {
  await open(page);
  await button(page, 'Settings').click();
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect(await acts(page)).toEqual(['new']);
});

test('Return finishes a dictation after Settings closes', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await button(page, 'Settings').click();
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
  await key(page, 'Enter');
  await expect.poll(() => acts(page)).toEqual(['new', 'record', 'done']);
});

for (const via of ['Escape', 'overlay']) {
  const close = async (page: Page): Promise<void> => {
    if (via === 'Escape') await key(page, 'Escape');
    else await page.mouse.click(5, 5);
    await nextFrame(page);
  };

  test(`Space dictates after a mouse user closes Settings (${via})`, async ({ page }) => {
    await open(page);
    await key(page, 'Meta+n');
    await expect(status(page)).toHaveText('Click Record and start talking.');
    await button(page, 'Settings').click();
    await close(page);
    await expect(button(page, 'Settings')).toBeFocused();
    await key(page, 'Space');
    await expect(status(page)).toContainText('Recording.');
    await expect(page.getByRole('dialog', { name: 'Settings' })).toHaveCount(0);
    expect(await acts(page)).toEqual(['new', 'record']);
  });

  test(`Space keeps the gear for a keyboard user (${via})`, async ({ page }) => {
    await open(page);
    await key(page, 'Meta+n');
    await expect(status(page)).toHaveText('Click Record and start talking.');
    await button(page, 'Settings').focus();
    await key(page, 'Enter');
    await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
    await close(page);
    await key(page, 'Space');
    await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
    expect(await acts(page)).toEqual(['new']);
  });
}

test('the gear gets the focus back even when its click did not focus it', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').evaluate((b) => b.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 })));
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  expect(await acts(page)).toEqual(['new', 'record']);
});

test('with an entry shown, Space after Settings closes goes to the gear', async ({ page }) => {
  await open(page, { library: '1' });
  await page.locator('#library li').first().getByRole('button').click();
  await button(page, 'Settings').click();
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
  await key(page, 'Space');
  await expect(page.getByRole('dialog', { name: 'Settings' })).toBeVisible();
});

test('a double-click on the shared primary slot pauses once, and cannot resume', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await button(page, 'Pause').dblclick();
  await expect(status(page)).toContainText('Paused');
  expect(await acts(page)).toEqual(['new', 'record', 'pause']);
  await button(page, 'Resume').evaluate((el) => el.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 })));
  await expect(status(page)).toContainText('Paused');
  expect(await acts(page)).toEqual(['new', 'record', 'pause']);
  await button(page, 'Resume').click();
  await expect(status(page)).toContainText('Recording.');
});

test('after mouse clicks, the keys still work because no button holds the focus', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await expect(status(page)).toContainText('Recording.');
  const onBody = (): Promise<boolean> => page.evaluate(() => document.activeElement === document.body);
  expect(await onBody()).toBe(true);
  await key(page, 'Space');
  await expect(status(page)).toContainText('Paused');
  expect(await onBody()).toBe(true);
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  expect(await onBody()).toBe(true);
  await key(page, 'Enter');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  expect(await acts(page)).toEqual(['new', 'record', 'pause', 'resume', 'done']);
});

test('a focused button that unmounts hands the keys back to the window', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Record').focus();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Paused');
  expect(await acts(page)).toEqual(['new', 'record', 'pause']);
});

test('a keycap is a small raised label, not bare text', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  const style = await button(page, 'Pause')
    .locator('kbd')
    .evaluate((el) => {
      const c = getComputedStyle(el);
      return {
        borderTopWidth: c.borderTopWidth,
        borderBottomWidth: c.borderBottomWidth,
        fontSize: c.fontSize,
        backgroundColor: c.backgroundColor,
      };
    });
  expect(style).toEqual({
    borderTopWidth: '1px',
    borderBottomWidth: '2px',
    fontSize: '11px',
    backgroundColor: 'rgba(0, 0, 0, 0)',
  });
});

test('the keycap sits at the same x for Record, Pause and Resume', async ({ page }) => {
  await open(page);
  const right = async (name: string): Promise<number> => {
    const box = await button(page, name).locator('kbd').boundingBox();
    if (box === null) throw new Error(`${name} has no keycap`);
    return box.x + box.width;
  };
  await press(page, 'New');
  const record = await right('Record');
  await press(page, 'Record');
  const pause = await right('Pause');
  await press(page, 'Pause');
  const resume = await right('Resume');
  expect(Math.abs(record - pause)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(pause - resume)).toBeLessThanOrEqual(0.5);
});

test('Discard sits left of Copy in finished, not stacked at one x', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  await expect(button(page, 'Copy')).toBeVisible();
  const discard = await button(page, 'Discard').boundingBox();
  const copy = await button(page, 'Copy').boundingBox();
  if (discard === null || copy === null) throw new Error('missing box');
  expect(discard.x + discard.width).toBeLessThanOrEqual(copy.x);
});

test.describe('at the minimum window size', () => {
  test.use({ viewport: { width: 520, height: 380 } });

  test('the controls and their keycaps fit, with the drawer open', async ({ page }) => {
    await open(page, { library: '30' });
    const check = async (): Promise<void> => {
      const buttons = page.locator('.controls button');
      const n = await buttons.count();
      expect(n).toBeGreaterThan(0);
      for (let i = 0; i < n; i++) {
        const b = buttons.nth(i);
        await expect(b).toBeInViewport({ ratio: 1 });
        const box = await b.boundingBox();
        const caps = b.locator('kbd');
        if (box === null) throw new Error('no button box');
        if ((await caps.count()) > 0) {
          const kbd = await caps.boundingBox();
          if (kbd === null) throw new Error('no keycap box');
          expect(kbd.x).toBeGreaterThanOrEqual(box.x - 0.5);
          expect(kbd.x + kbd.width).toBeLessThanOrEqual(box.x + box.width + 0.5);
          expect(kbd.y).toBeGreaterThanOrEqual(box.y - 0.5);
          expect(kbd.y + kbd.height).toBeLessThanOrEqual(box.y + box.height + 0.5);
        }
      }
      const overflow = await page.evaluate(() => ({
        x: document.documentElement.scrollWidth - window.innerWidth,
        y: document.documentElement.scrollHeight - window.innerHeight,
      }));
      expect(overflow.x).toBeLessThanOrEqual(0);
      expect(overflow.y).toBeLessThanOrEqual(0);
    };
    await press(page, 'New');
    await check();
    await press(page, 'Record');
    await check();
    await press(page, 'Pause');
    await check();
  });
});

// ---- the microphone menu ---------------------------------------------------------

/** The microphone control. */
function mic(page: Page) {
  return page.getByRole('button', { name: /^Microphone/ });
}

/** Starts recording, then opens the menu from the keyboard (Tab, Return) with an item focused. */
async function recordingWithMenu(page: Page): Promise<void> {
  await open(page);
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Tab');
  await expect(mic(page)).toBeFocused();
  await key(page, 'Enter');
  await expect(page.getByRole('menuitemradio', { name: 'Mac default (Jabra Evolve 65)' })).toBeFocused();
  await key(page, 'ArrowDown');
  await expect(page.getByRole('menuitemradio', { name: 'AirPods Pro' })).toBeFocused();
}

async function savedInputs(page: Page): Promise<string[]> {
  return page.evaluate(() => (window.__mockBridge?.saved ?? []).map((s) => s.inputDevice));
}

for (const name of ['Space', 'Enter'] as const) {
  test(`${name} in the microphone menu chooses the item rather than dictating`, async ({ page }) => {
    await recordingWithMenu(page);
    await key(page, name);
    await expect(page.getByRole('menu')).toHaveCount(0);
    await expect.poll(() => savedInputs(page)).toEqual(['AirPods Pro']);
    expect(await acts(page), 'no pause, no done').toEqual(['new', 'record']);
    await expect(status(page)).toHaveText(/^Recording\./);
  });
}

test('after a right-click and Escape, Space still pauses', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await expect(status(page)).toContainText('Recording.');
  await mic(page).click({ button: 'right' });
  await expect(page.getByRole('menu')).toBeVisible();
  await key(page, 'Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect(mic(page)).not.toBeFocused();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Paused');
});

test('Tab to the microphone, Return, Escape: the focus is back on the control', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await key(page, 'Tab');
  await expect(mic(page)).toBeFocused();
  await key(page, 'Enter');
  await expect(page.getByRole('menu')).toBeVisible();
  await key(page, 'Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect(mic(page)).toBeFocused();
});

// ---- ⌘, opens Settings -----------------------------------------------------------

async function listings(page: Page): Promise<number | undefined> {
  return page.evaluate(() => window.__mockBridge?.modelListings);
}

const settings = (page: Page) => page.getByRole('dialog', { name: 'Settings' });
const vocabulary = (page: Page) => page.getByRole('textbox', { name: 'Vocabulary file' });

test('⌘, opens Settings when ready, without acting', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  await expect(vocabulary(page)).toBeFocused();
  expect(await acts(page)).toEqual(['new']);
});

test('⌘, opens Settings while recording, and Space dictates again after Escape', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await expect(status(page)).toContainText('Recording.');
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  expect(await acts(page)).toEqual(['new', 'record']);
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
  await key(page, 'Space');
  await expect.poll(() => acts(page)).toEqual(['new', 'record', 'pause']);
});

test('⌘, opens Settings while paused', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Pause');
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  expect(await acts(page)).toEqual(['new', 'record', 'pause']);
});

test('⌘, opens Settings when idle', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  expect(await acts(page)).toEqual([]);
});

test('⌘, opens Settings while an action is in flight', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await hold(page, 'dictation_act');
  await key(page, 'Space');
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  await key(page, 'Escape');
  await release(page, 'dictation_act');
  await expect(status(page)).toContainText('Recording.');
  expect(await acts(page)).toEqual(['new', 'record']);
});

test('⌘, opens Settings before the session has loaded', async ({ page }) => {
  await open(page, { hold: 'dictation_state' });
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  expect(await acts(page)).toEqual([]);
  await key(page, 'Escape');
  await release(page, 'dictation_state');
  await expect(status(page)).toHaveText('Click New to start a dictation.');
});

test('⌘, opens Settings while a library entry is shown', async ({ page }) => {
  await open(page, { library: '1' });
  await key(page, 'Meta+n');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await page.locator('#library li').first().getByRole('button').click();
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is preloaded recording number 1.');
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  expect(await acts(page)).toEqual(['new']);
});

test('⌘, in the transcript opens Settings, leaves the text, and the gear gets the focus', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toBeEditable();
  await expect(transcript).not.toHaveValue('');
  const before = await transcript.inputValue();
  await transcript.focus();
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  await expect(transcript).toHaveValue(before);
  await key(page, 'Escape');
  await nextFrame(page);
  await expect(button(page, 'Settings')).toBeFocused();
});

test('⌘, lists the models once and does nothing while Settings is open', async ({ page }) => {
  await open(page);
  expect(await listings(page)).toBe(0);
  await key(page, 'Meta+Comma');
  await expect(settings(page)).toBeVisible();
  const model = page.getByRole('combobox', { name: 'Title model' });
  await expect(model).toBeEnabled();
  await expect.poll(() => model.locator('option').allTextContents()).toEqual(['gemma3:4b', 'qwen3.8:27b-mlx']);
  expect(await listings(page)).toBe(1);
  await key(page, 'Meta+Comma');
  await nextFrame(page);
  expect(await listings(page)).toBe(1);
  await expect(settings(page)).toHaveCount(1);
  await expect(vocabulary(page)).toBeFocused();
});

test('⌘, in the microphone menu does nothing', async ({ page }) => {
  await recordingWithMenu(page);
  await key(page, 'Meta+Comma');
  await nextFrame(page);
  await expect(settings(page)).toHaveCount(0);
  await expect(page.getByRole('menuitemradio', { name: 'AirPods Pro' })).toBeFocused();
});

test('a repeated ⌘, does not open Settings', async ({ page }) => {
  await open(page);
  await page.evaluate(() => {
    window.dispatchEvent(new KeyboardEvent('keydown', { key: ',', metaKey: true, repeat: true, bubbles: true, cancelable: true }));
  });
  await nextFrame(page);
  await expect(settings(page)).not.toBeVisible();
  expect(await listings(page)).toBe(0);
});

test('⌘, has its default cancelled', async ({ page }) => {
  await open(page);
  await page.evaluate(() => {
    const seen: boolean[] = [];
    (window as unknown as { __commaPrevented: boolean[] }).__commaPrevented = seen;
    window.addEventListener('keydown', (e) => {
      if (e.key === ',') seen.push(e.defaultPrevented);
    });
  });
  await key(page, 'Meta+Comma');
  expect(await page.evaluate(() => (window as unknown as { __commaPrevented: boolean[] }).__commaPrevented)).toEqual([true]);
});

test('the gear names ⌘, for assistive tech and in its tooltip', async ({ page }) => {
  await open(page);
  await expect(button(page, 'Settings')).toHaveAttribute('aria-keyshortcuts', 'Meta+Comma');
  await expect(button(page, 'Settings')).toHaveAttribute('title', 'Settings (⌘,)');
  await expect(button(page, 'Settings').locator('kbd')).toHaveCount(0);
});

test('Space pauses and resumes while the Heard so far region has focus', async ({ page }) => {
  await open(page);
  await key(page, 'Meta+n');
  await key(page, 'Space');
  await key(page, 'Space');
  const region = page.getByRole('region', { name: 'Heard so far' });
  await expect(region).toBeVisible();
  await region.focus();
  await key(page, 'Space');
  await expect(status(page)).toContainText('Recording.');
  expect((await acts(page)).at(-1)).toBe('resume');
});
