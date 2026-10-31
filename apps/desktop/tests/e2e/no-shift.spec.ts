import { expect, test } from './fixtures';

import { button, expectNoShift, expectSameBox, hold, open, press, release, status } from './helpers';

// No layout shift within a screen state (CLAUDE.md). Left alone, with reasons:
// CopyButton is position: absolute and 28x28, and its check swaps one 16x16 svg
// for another. The microphone popover is position: fixed, outside main's flow,
// and its trigger name is nowrap/ellipsis. The band's .time grows into
// .band-spacer. loadError and the drawer's empty line are whole-screen or
// list-content states. The download block is the last element of main.

const SIZES = [
  { width: 760, height: 560 },
  { width: 520, height: 380 },
];
const PART_1 = 'This is part 1 of the dictation.';
const PART_2 = 'This is part 2 of the dictation.';
const FIVE = 'This IS part 1 OF the Dictation.\n\nThis is PART 2 of THE dictation.';
const ONE = PART_1.replace('dictation', 'Dictation');
const ONE_LEARNED = 'Copied to the clipboard. Learned: Dictation.';

type Page = import('@playwright/test').Page;

async function rowHeight(page: Page): Promise<void> {
  expect((await page.locator('.status-row').first().boundingBox())?.height).toBe(20);
}

async function learnOne(page: Page, params: Record<string, string> = {}): Promise<void> {
  await open(page, params);
  await press(page, 'New', 'Record', 'Done');
  await page.locator('#transcript').fill(ONE);
}

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`${colorScheme} mode`, () => {
    test.use({ colorScheme });
    for (const size of SIZES) {
      test.describe(`${String(size.width)}x${String(size.height)}`, () => {
        test('learn one', async ({ page }) => {
          await page.setViewportSize(size);
          await learnOne(page);
          const t = page.locator('#transcript');
          const c = page.locator('.controls');
          await expectNoShift(page, t, async () => {
            await expectNoShift(page, c, async () => {
              await press(page, 'Copy');
              await expect(status(page)).toHaveText(ONE_LEARNED);
            });
          });
          await rowHeight(page);
        });

        test('learn five', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page);
          await press(page, 'New', 'Record', 'Pause', 'Resume', 'Done');
          await expect(page.locator('#transcript')).toHaveValue(`${PART_1}\n\n${PART_2}`);
          await page.locator('#transcript').fill(FIVE);
          await expectNoShift(page, page.locator('#transcript'), async () => {
            await press(page, 'Copy');
            await expect(status(page)).toHaveText('Copied to the clipboard. Learned: IS, OF, Dictation, PART, THE.');
          });
          const undo = button(page, 'Undo');
          await expect(undo).toBeVisible();
          await expect(undo).toBeInViewport();
          const u = await undo.boundingBox();
          const r = await page.locator('.status-row').first().boundingBox();
          expect((u?.x ?? 0) + (u?.width ?? 0)).toBeLessThanOrEqual((r?.x ?? 0) + (r?.width ?? 0));
          expect(await status(page).evaluate((e) => e.scrollHeight <= e.clientHeight)).toBe(true);
          await rowHeight(page);
        });

        test('undo', async ({ page }) => {
          await page.setViewportSize(size);
          await learnOne(page);
          await press(page, 'Copy');
          await expect(status(page)).toHaveText(ONE_LEARNED);
          await expectNoShift(page, page.locator('#transcript'), async () => {
            await expectNoShift(page, page.locator('.status-row').first(), async () => {
              await press(page, 'Undo');
              await expect(button(page, 'Undo')).toHaveCount(0);
              await expect(status(page)).toHaveText('Copied to the clipboard.');
            });
          });
          await rowHeight(page);
        });

        test('failing copy', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { clipboard: 'fail' });
          await press(page, 'New', 'Record', 'Done');
          const alert = page.getByRole('main').getByRole('alert');
          await expectNoShift(page, page.locator('#transcript'), async () => {
            await press(page, 'Copy');
            await expect(alert).toHaveText('Could not copy to the clipboard: pasteboard unavailable');
          });
          await expect(status(page)).toHaveClass(/visually-hidden/);
          await expect(status(page)).toHaveCSS('position', 'absolute');
          const a = await alert.boundingBox();
          const r = await page.locator('.status-row').first().boundingBox();
          expect(a?.x).toBe(r?.x);
          expect(a?.width).toBe(r?.width);
          await rowHeight(page);
        });

        test('input fallback', async ({ page }) => {
          await page.setViewportSize(size);
          const b = await page.context().newPage();
          await b.setViewportSize(size);
          await open(page, { inputDevice: 'Gone Mic' });
          await open(b);
          await press(page, 'New', 'Record');
          await press(b, 'New', 'Record');
          await expect(status(page)).toContainText('Gone Mic is not available');
          for (const sel of ['.status-row', '.controls', '.meter']) {
            await expectSameBox(page.locator(sel).first(), b.locator(sel).first());
          }
          if (size.width === 520) {
            expect(await status(page).evaluate((e) => e.scrollWidth > e.clientWidth)).toBe(true);
          }
          await rowHeight(page);
          await b.close();
        });

        test('band error', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { fail: 'settings_set' });
          await press(page, 'New');
          const targets = [button(page, 'New'), button(page, 'Settings'), page.locator('.controls')];
          const before = [];
          for (const t of targets) before.push(await t.boundingBox());
          await page.getByRole('button', { name: /^Microphone/ }).click();
          await page.getByRole('menuitemradio', { name: 'AirPods Pro' }).click();
          await expect(page.locator('.band').getByRole('alert')).toHaveText('Mock failure: settings_set');
          for (const [i, t] of targets.entries()) expect(await t.boundingBox()).toEqual(before[i]);
          await rowHeight(page);
        });

        test('retry download', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { model: 'fail' });
          await expect(page.getByRole('alert')).toHaveText('Could not download the speech model: the connection was reset.');
          await hold(page, 'download');
          const row = page.locator('.status-row').first();
          const controls = page.locator('.controls');
          const [b1, b2] = [await row.boundingBox(), await controls.boundingBox()];
          await press(page, 'Retry download');
          await expect(page.getByRole('alert')).toHaveCount(0);
          await expect(page.getByRole('progressbar')).toHaveAttribute('value', '0');
          expect(await row.boundingBox()).toEqual(b1);
          expect(await controls.boundingBox()).toEqual(b2);
          const d = await page.locator('.download').boundingBox();
          expect(d?.y ?? 0).toBeGreaterThanOrEqual((b2?.y ?? 0) + (b2?.height ?? 0));
          await expect(status(page)).not.toHaveClass(/visually-hidden/);
          await release(page, 'download');
          await expect(page.getByRole('progressbar')).toHaveCount(0);
          await rowHeight(page);
        });

        test('entry copy failure', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { fail: 'library_copy', library: '1' });
          await page.locator('#library li').first().getByRole('button').click();
          await expectNoShift(page, page.locator('#entry-transcript'), async () => {
            await press(page, 'Copy');
            await expect(page.getByRole('main').getByRole('alert')).toHaveText('Mock failure: library_copy');
          });
          await rowHeight(page);
        });

        test('settings note', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { ollama: 'down', hold: 'ollama_models' });
          await button(page, 'Settings').click();
          await expectNoShift(page, page.getByRole('dialog'), async () => {
            await expectNoShift(page, page.locator('#summary-model'), async () => {
              await release(page, 'ollama_models');
              await expect(page.getByText('Local AI needs Ollama. Start Ollama, then reopen Settings.')).toBeVisible();
            });
          });
          expect(await page.locator('.settings .note').evaluate((e) => e.scrollWidth <= e.clientWidth)).toBe(true);
          await rowHeight(page);
        });

        test('settings save failure', async ({ page }) => {
          await page.setViewportSize(size);
          await open(page, { fail: 'settings_set' });
          await button(page, 'Settings').click();
          const dialog = page.getByRole('dialog');
          await expectNoShift(page, dialog, async () => {
            await dialog.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' }).click();
            await expect(dialog.getByRole('alert')).toHaveText('Mock failure: settings_set');
          });
          await rowHeight(page);
        });

        test('failed undo', async ({ page }) => {
          await page.setViewportSize(size);
          await learnOne(page, { fail: 'undoLearn' });
          await press(page, 'Copy');
          await expect(status(page)).toHaveText(ONE_LEARNED);
          await expectNoShift(page, page.locator('#transcript'), async () => {
            await press(page, 'Undo');
            await expect(page.getByRole('main').getByRole('alert')).toHaveText('Mock failure: undoLearn');
          });
          await expect(button(page, 'Undo')).toBeInViewport();
          await rowHeight(page);
        });
      });
    }
  });
}
