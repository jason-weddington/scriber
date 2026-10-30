import type { Locator, Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { button, expectEveryItemReachable, hold, open, press, release, status } from './helpers';

// The microphone control above the record controls, and its in-page menu of
// input devices (docs/design.md, "Microphone."): what it names, what the
// menu lists and checks, what a choice saves, how the listing behaves, the
// keyboard path, the fallback notice, and the popover's place at small sizes.

const DEFAULT_ITEM = 'Mac default (Jabra Evolve 65)';

function mic(page: Page): Locator {
  return page.getByRole('button', { name: /^Microphone/ });
}

function menu(page: Page): Locator {
  return page.getByRole('menu', { name: 'Microphone' });
}

function items(page: Page): Locator {
  return page.getByRole('menuitemradio');
}

function item(page: Page, name: string): Locator {
  return page.getByRole('menuitemradio', { name, exact: true });
}

/** The inputDevice of every settings_set call, in order. */
async function savedInputs(page: Page): Promise<string[]> {
  return page.evaluate(() => (window.__mockBridge?.saved ?? []).map((s) => s.inputDevice));
}

async function inputListings(page: Page): Promise<number> {
  return page.evaluate(() => window.__mockBridge?.inputListings ?? -1);
}

async function openMenu(page: Page): Promise<void> {
  await mic(page).click();
  await expect(menu(page)).toBeVisible();
}

async function choose(page: Page, name: string): Promise<void> {
  await openMenu(page);
  await item(page, name).click();
  await expect(menu(page)).toHaveCount(0);
}

// ---- where the control shows ------------------------------------------------------

test('the control shows in ready, recording and paused, and nowhere else', async ({ page }) => {
  await open(page);
  await expect(status(page)).toHaveText('Click New to start a dictation.');
  await expect(mic(page), 'idle').toHaveCount(0);
  await press(page, 'New');
  await expect(mic(page), 'ready').toHaveCount(1);
  await press(page, 'Record');
  await expect(mic(page), 'recording').toHaveCount(1);
  await press(page, 'Pause');
  await expect(mic(page), 'paused').toHaveCount(1);
  await press(page, 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  await expect(mic(page), 'finished').toHaveCount(0);
});

test('the control is not shown when the session failed', async ({ page }) => {
  await open(page, { leftover: '1' });
  await expect(button(page, 'Retry')).toBeVisible();
  await expect(mic(page)).toHaveCount(0);
});

test('the control is not shown while a library entry is', async ({ page }) => {
  await open(page, { library: '30' });
  await press(page, 'New');
  await expect(mic(page)).toHaveCount(1);
  await page.locator('#library li').first().getByRole('button').click();
  await expect(page.getByRole('heading', { level: 2 })).toBeVisible();
  await expect(mic(page)).toHaveCount(0);
});

test('the control names the microphone, with the full name as its tooltip', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect(mic(page)).toHaveAccessibleName('Microphone: Jabra Evolve 65');
  await expect(mic(page)).toHaveText('Jabra Evolve 65');
  await expect(mic(page)).toHaveAttribute('title', 'Jabra Evolve 65');
  await expect(mic(page)).toHaveAttribute('aria-haspopup', 'menu');
  await expect(mic(page)).toHaveAttribute('aria-expanded', 'false');
  await openMenu(page);
  await expect(mic(page)).toHaveAttribute('aria-expanded', 'true');
});

test('with no input device the control says Microphone and the menu offers only the Mac default', async ({ page }) => {
  await open(page, { inputs: 'none' });
  await press(page, 'New');
  await expect(mic(page)).toHaveAccessibleName('Microphone');
  await expect(mic(page)).toHaveText('Microphone');
  await expect(mic(page)).not.toHaveAttribute('title');
  await openMenu(page);
  await expect(items(page)).toHaveText([/^✓\s*Mac default$/]);
  await expect(item(page, 'Mac default')).toHaveAttribute('aria-checked', 'true');
});

// ---- what the menu lists and checks ------------------------------------------------

test('the menu lists the Mac default first, then every other device, the choice checked', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await openMenu(page);
  await expect(items(page)).toHaveText([DEFAULT_ITEM, 'AirPods Pro', 'MacBook Pro Microphone'].map((t) => new RegExp(`${t.replace(/[()]/g, '\\$&')}$`)));
  await expect(page.locator('[role="menuitemradio"][aria-checked="true"]')).toHaveCount(1);
  await expect(item(page, DEFAULT_ITEM)).toHaveAttribute('aria-checked', 'true');
  await expect(item(page, DEFAULT_ITEM).locator('.check')).toHaveText('✓');
  await expect(item(page, 'AirPods Pro').locator('.check')).toHaveText('');
  await expect(item(page, 'AirPods Pro')).toHaveAttribute('title', 'AirPods Pro');
});

test('a saved device is checked, and a missing one leaves the Mac default checked', async ({ page }) => {
  await open(page, { inputDevice: 'AirPods Pro' });
  await press(page, 'New');
  await openMenu(page);
  await expect(item(page, 'AirPods Pro')).toHaveAttribute('aria-checked', 'true');
  await expect(page.locator('[role="menuitemradio"][aria-checked="true"]')).toHaveCount(1);
  const gone = await page.context().newPage();
  await open(gone, { inputDevice: 'Gone Mic' });
  await press(gone, 'New');
  await openMenu(gone);
  await expect(item(gone, DEFAULT_ITEM)).toHaveAttribute('aria-checked', 'true');
  await expect(gone.locator('[role="menuitemradio"][aria-checked="true"]')).toHaveCount(1);
});

// ---- choosing ------------------------------------------------------------------------

test('choosing a device saves it, and choosing the Mac default saves an empty name', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await choose(page, 'AirPods Pro');
  await expect(mic(page)).toHaveAccessibleName('Microphone: AirPods Pro');
  await expect.poll(async () => (await savedInputs(page)).at(-1)).toBe('AirPods Pro');
  await choose(page, DEFAULT_ITEM);
  await expect.poll(async () => (await savedInputs(page)).at(-1)).toBe('');
  await expect(mic(page)).toHaveAccessibleName('Microphone: Jabra Evolve 65');
});

test('choosing the checked item again sends nothing', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await choose(page, 'AirPods Pro');
  await expect.poll(() => savedInputs(page)).toEqual(['AirPods Pro']);
  await choose(page, 'AirPods Pro');
  await choose(page, DEFAULT_ITEM);
  await choose(page, DEFAULT_ITEM);
  await expect.poll(() => savedInputs(page)).toEqual(['AirPods Pro', '']);
});

test('a choice that cannot be saved is undone and says why', async ({ page }) => {
  await open(page, { fail: 'settings_set' });
  await press(page, 'New');
  await choose(page, 'AirPods Pro');
  await expect(page.getByRole('alert')).toHaveText('Mock failure: settings_set');
  await expect(mic(page)).toHaveAccessibleName('Microphone: Jabra Evolve 65');
});

test('choosing the Mac default over a missing device saves it, and Record gives no notice', async ({ page }) => {
  await open(page, { inputDevice: 'Gone Mic' });
  await press(page, 'New');
  await choose(page, DEFAULT_ITEM);
  await expect.poll(() => savedInputs(page)).toEqual(['']);
  await press(page, 'Record');
  await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
});

test('a choice made while recording names the open device until the next start', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record');
  await choose(page, 'AirPods Pro');
  await expect.poll(async () => (await savedInputs(page)).at(-1)).toBe('AirPods Pro');
  await expect(mic(page)).toHaveAccessibleName('Microphone: Jabra Evolve 65');
  await press(page, 'Pause');
  await expect(mic(page)).toHaveAccessibleName('Microphone: AirPods Pro');
  await press(page, 'Resume');
  await expect(status(page)).toContainText('Recording.');
  await expect(mic(page)).toHaveAccessibleName('Microphone: AirPods Pro');
});

test('a choice made while paused is named at once and opened by Resume', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Pause');
  await choose(page, 'AirPods Pro');
  await expect(mic(page)).toHaveAccessibleName('Microphone: AirPods Pro');
  await press(page, 'Resume');
  await expect(status(page)).toContainText('Recording.');
  await expect(mic(page)).toHaveAccessibleName('Microphone: AirPods Pro');
});

// ---- the fallback --------------------------------------------------------------------

test('a missing device records from the Mac default and says so, keeping the choice', async ({ page }) => {
  await open(page, { inputDevice: 'Gone Mic' });
  await press(page, 'New', 'Record');
  await expect(status(page)).toHaveText('Gone Mic is not available, so this part is recording from the Mac default, Jabra Evolve 65.');
  await expect(mic(page)).toHaveAccessibleName('Microphone: Jabra Evolve 65');
  // settings_get still answers the choice: the fallback never changes it.
  const saved = await page.evaluate(async () => {
    const path = '/src/lib/bridge.ts';
    const bridge = (await import(/* @vite-ignore */ path)) as { settingsGet: () => Promise<{ inputDevice: string }> };
    return (await bridge.settingsGet()).inputDevice;
  });
  expect(saved).toBe('Gone Mic');
  expect(await savedInputs(page)).toEqual([]);
});

// ---- the listing -----------------------------------------------------------------------

test('a failed listing says why inside the menu and offers the Mac default', async ({ page }) => {
  await open(page, { fail: 'input_devices' });
  await press(page, 'New');
  await openMenu(page);
  await expect(page.locator('.mic-popover').getByRole('alert')).toHaveText('Mock failure: input_devices');
  await expect(menu(page).getByRole('alert')).toHaveCount(0);
  await expect(items(page)).toHaveText([/^✓\s*Mac default$/]);
});

test('the devices are listed when New reaches ready and each time the menu opens', async ({ page }) => {
  await open(page);
  await expect.poll(() => inputListings(page)).toBe(1);
  await press(page, 'New');
  await expect.poll(() => inputListings(page)).toBe(2);
  await openMenu(page);
  await expect.poll(() => inputListings(page)).toBe(3);
});

test('a held listing leaves the menu open and usable, and its answer keeps the focus', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect.poll(() => inputListings(page)).toBe(2);
  await hold(page, 'input_devices');
  await openMenu(page);
  await expect(item(page, DEFAULT_ITEM)).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(item(page, 'AirPods Pro')).toBeFocused();
  await release(page, 'input_devices');
  await expect.poll(() => inputListings(page)).toBe(3);
  await expect(item(page, 'AirPods Pro')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect.poll(() => savedInputs(page)).toEqual(['AirPods Pro']);
});

test('two listings in flight settle into one list', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await expect.poll(() => inputListings(page)).toBe(2);
  await hold(page, 'input_devices');
  await openMenu(page);
  await page.keyboard.press('Escape');
  await page.evaluate(() => {
    window.__mockBridge?.setInputs(['Newer Mic']);
  });
  await openMenu(page);
  await release(page, 'input_devices');
  await expect.poll(() => inputListings(page)).toBe(4);
  await expect(items(page)).toHaveCount(2);
  await expect(item(page, 'Newer Mic')).toBeVisible();
});

test('a listing that drops the focused device moves the focus to the checked item', async ({ page }) => {
  await open(page, { inputDevice: 'AirPods Pro' });
  await press(page, 'New');
  await openMenu(page);
  await expect(item(page, 'AirPods Pro')).toBeFocused();
  await page.keyboard.press('Escape');
  await page.evaluate(() => {
    window.__mockBridge?.setInputs(['MacBook Pro Microphone']);
  });
  await openMenu(page);
  await expect(item(page, 'AirPods Pro')).toHaveCount(0);
  await expect(page.locator('[role="menuitemradio"]:focus')).toHaveCount(1);
  await expect(item(page, DEFAULT_ITEM)).toBeFocused();
});

// ---- the keyboard and the pointer ------------------------------------------------------

test('opening focuses the checked item, and the arrows, Home and End move and wrap', async ({ page }) => {
  await open(page, { inputDevice: 'AirPods Pro' });
  await press(page, 'New');
  await openMenu(page);
  await expect(item(page, 'AirPods Pro')).toBeFocused();
  await page.keyboard.press('End');
  await expect(item(page, 'MacBook Pro Microphone')).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(item(page, DEFAULT_ITEM)).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(item(page, 'MacBook Pro Microphone')).toBeFocused();
  await page.keyboard.press('Home');
  await expect(item(page, DEFAULT_ITEM)).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(item(page, 'MacBook Pro Microphone')).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(item(page, 'AirPods Pro')).toBeFocused();
});

test('Tab closes the menu', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await openMenu(page);
  await page.keyboard.press('Tab');
  await expect(menu(page)).toHaveCount(0);
});

test('a pointerdown outside closes the menu, and so does a click on the control', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await openMenu(page);
  await status(page).dispatchEvent('pointerdown');
  await expect(menu(page)).toHaveCount(0);
  await openMenu(page);
  await mic(page).click();
  await expect(menu(page)).toHaveCount(0);
});

test('a right-click opens the menu, and another leaves it open', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await mic(page).click({ button: 'right' });
  await expect(menu(page)).toBeVisible();
  await mic(page).click({ button: 'right' });
  await expect(menu(page)).toBeVisible();
});

test('resizing the window closes the menu', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await openMenu(page);
  await page.setViewportSize({ width: 700, height: 500 });
  await expect(menu(page)).toHaveCount(0);
});

// ---- reach and geometry ------------------------------------------------------------------

/** The popover lies wholly inside the viewport. */
async function expectPopoverInside(page: Page, label: string): Promise<void> {
  await openMenu(page);
  const box = await page.locator('.mic-popover').boundingBox();
  const size = page.viewportSize();
  if (box === null || size === null) throw new Error('no popover box');
  expect(box.x, `${label}: left`).toBeGreaterThanOrEqual(0);
  expect(box.y, `${label}: top`).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width, `${label}: right`).toBeLessThanOrEqual(size.width);
  expect(box.y + box.height, `${label}: bottom`).toBeLessThanOrEqual(size.height);
  await page.keyboard.press('Escape');
  await expect(menu(page)).toHaveCount(0);
}

test('every device in a long list can be reached', async ({ page }) => {
  await open(page, { inputs: 'many' });
  await press(page, 'New');
  await openMenu(page);
  await expectEveryItemReachable(items(page));
});

test.describe('at the minimum window size', () => {
  test.use({ viewport: { width: 520, height: 380 } });

  test('a long device name is cut with an ellipsis and named in full in its tooltip', async ({ page }) => {
    await open(page, { inputs: 'long', inputDevice: 'L'.repeat(80) });
    await press(page, 'New');
    const name = mic(page).locator('.mic-name');
    await expect(name).toHaveCSS('text-overflow', 'ellipsis');
    expect(await name.evaluate((e) => e.scrollWidth > e.clientWidth)).toBe(true);
    await expect(mic(page)).toHaveAttribute('title', 'L'.repeat(80));
    await openMenu(page);
    const label = item(page, 'L'.repeat(80)).locator('.mic-item');
    expect(await label.evaluate((e) => e.scrollWidth > e.clientWidth)).toBe(true);
  });

  test('every device in a long list can be reached', async ({ page }) => {
    await open(page, { inputs: 'many' });
    await press(page, 'New');
    await openMenu(page);
    await expectEveryItemReachable(items(page));
  });

  for (const inputs of ['many', 'long']) {
    test(`the popover stays inside the window (${inputs})`, async ({ page }) => {
      await open(page, { library: '30', inputs });
      await press(page, 'New');
      await expectPopoverInside(page, 'ready');
      await press(page, 'Record');
      await expectPopoverInside(page, 'recording');
    });

    test(`the popover stays inside the window with the drawer at its widest (${inputs})`, async ({ page }) => {
      await open(page, { library: '30', inputs, libraryWidth: '440' });
      await press(page, 'New');
      await expectPopoverInside(page, 'ready');
      await press(page, 'Record');
      await expectPopoverInside(page, 'recording');
    });
  }

  test('the first and the last device can be clicked while recording', async ({ page }) => {
    await open(page, { inputs: 'many', inputDevice: 'Microphone 15' });
    await press(page, 'New', 'Record');
    await openMenu(page);
    await items(page).first().click();
    await expect(menu(page)).toHaveCount(0);
    await expect.poll(() => savedInputs(page)).toEqual(['']);
    await openMenu(page);
    await items(page).last().click();
    await expect(menu(page)).toHaveCount(0);
    await expect.poll(() => savedInputs(page)).toEqual(['', 'Microphone 30']);
  });

  test('low in the window the popover opens upward, above the control', async ({ page }) => {
    await open(page, { library: '30', inputDevice: 'Gone Mic' });
    await press(page, 'New', 'Record');
    await expect(status(page)).toContainText('Gone Mic is not available');
    await openMenu(page);
    const control = await mic(page).boundingBox();
    const box = await page.locator('.mic-popover').boundingBox();
    if (control === null || box === null) throw new Error('no box');
    expect(box.y + box.height).toBeLessThanOrEqual(control.y);
    expect(box.y).toBeGreaterThanOrEqual(0);
  });

  test('the record controls stay in the window under the fallback notice', async ({ page }) => {
    await open(page, { library: '30', inputDevice: 'Gone Mic' });
    await press(page, 'New', 'Record');
    await expect(status(page)).toContainText('Gone Mic is not available');
    const buttons = page.locator('.controls button');
    const n = await buttons.count();
    expect(n).toBeGreaterThan(0);
    for (let i = 0; i < n; i++) await expect(buttons.nth(i)).toBeInViewport({ ratio: 1 });
  });
});
