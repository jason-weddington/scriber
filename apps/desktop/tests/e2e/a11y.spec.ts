import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';

import { expect, test } from './fixtures';

import { hold, open, press, release, status } from './helpers';

// axe-core over every state the window can be in, in light and dark. Any
// violation fails, whatever its impact: a contrast miss in dark mode is as
// real as a missing label.

async function expectNoViolations(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help} (${String(v.nodes.length)})`)).toEqual([]);
}

/**
 * Keeps Copy's green check up. It clears after 2 s and a scan can take longer
 * on a busy host, which would scan the wrong state. A clock cannot hold it:
 * axe waits on timers of its own, so only that one timer is dropped.
 * Each dropped timer is counted in window.__droppedCopyTimers; the copied tests
 * assert exactly one drop per Copy press, so they fail if CopyButton stops using
 * a 2000 ms setTimeout. That the check clears after 2 s is covered by the
 * fake-clock tests in dictation.spec.ts (copiedAt60s).
 */
async function keepCheck(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const w = window as unknown as { __droppedCopyTimers: number };
    w.__droppedCopyTimers = 0;
    const real = window.setTimeout.bind(window);
    window.setTimeout = ((fn: TimerHandler, ms?: number, ...rest: unknown[]) => {
      if (ms === 2000) {
        w.__droppedCopyTimers++;
        return 0;
      }
      return real(fn, ms, ...rest);
    }) as typeof window.setTimeout;
  });
}

function droppedTimers(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as { __droppedCopyTimers: number }).__droppedCopyTimers);
}

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`${colorScheme} mode`, () => {
    test.use({ colorScheme });

    test('finished, just copied', async ({ page }) => {
      await keepCheck(page);
      await open(page);
      await press(page, 'New', 'Record', 'Done');
      const before = await droppedTimers(page);
      await press(page, 'Copy');
      await expect(page.locator('button.copy.copied')).toBeVisible();
      expect((await droppedTimers(page)) - before).toBe(1);
      await expectNoViolations(page);
    });

    test('finished, having learned a word', async ({ page }) => {
      await keepCheck(page);
      await open(page);
      await press(page, 'New', 'Record', 'Done');
      await page.getByRole('textbox', { name: 'Transcript' }).fill('This is part 1 of the Dictation.');
      await press(page, 'Copy');
      await expect(page.getByText('Learned: Dictation')).toBeVisible();
      await expect(page.getByRole('button', { name: 'Undo' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('finished, having learned five words', async ({ page }) => {
      await keepCheck(page);
      await open(page);
      await press(page, 'New', 'Record', 'Pause', 'Resume', 'Done');
      await page
        .getByRole('textbox', { name: 'Transcript' })
        .fill('This IS part 1 OF the Dictation.\n\nThis is PART 2 of THE dictation.');
      await press(page, 'Copy');
      await expect(page.getByText('Copied to the clipboard. Learned: IS, OF, Dictation, PART, THE.')).toBeVisible();
      await expect(page.getByRole('button', { name: 'Undo' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('a library entry just copied', async ({ page }) => {
      await keepCheck(page);
      await open(page, { library: '3' });
      await page.locator('#library li').nth(1).getByRole('button').click();
      const before = await droppedTimers(page);
      await press(page, 'Copy');
      await expect(page.locator('button.copy.copied')).toBeVisible();
      expect((await droppedTimers(page)) - before).toBe(1);
      await expectNoViolations(page);
    });

    test('idle', async ({ page }) => {
      await open(page);
      await expect(status(page)).toHaveText('Click New to start a dictation.');
      await expectNoViolations(page);
    });

    test('the window in full screen', async ({ page }) => {
      await open(page, { fullscreen: '1' });
      await expect(page.locator('.band')).toHaveClass(/\bfullscreen\b/);
      await expectNoViolations(page);
    });

    test('ready', async ({ page }) => {
      await open(page);
      await press(page, 'New');
      await expect(status(page)).toHaveText('Click Record and start talking.');
      await expectNoViolations(page);
    });

    test('recording', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record');
      await expect(status(page)).toContainText('Recording.');
      await expectNoViolations(page);
    });

    test('paused, transcribing in the background', async ({ page }) => {
      await open(page);
      await hold(page, 'transcribe');
      await press(page, 'New', 'Record', 'Pause');
      await expect(status(page)).toContainText('Transcribing 1 part');
      await expectNoViolations(page);
      await release(page, 'transcribe');
    });

    test('transcribing', async ({ page }) => {
      await open(page);
      await hold(page, 'transcribe');
      await press(page, 'New', 'Record', 'Done');
      await expect(status(page)).toContainText('Transcribing the last');
      await expectNoViolations(page);
      await release(page, 'transcribe');
    });

    test('transcribing, waiting for the cleanup', async ({ page }) => {
      await open(page, { cleanup: 'on', hold: 'cleanup' });
      await press(page, 'New', 'Record', 'Done');
      await expect(status(page)).toHaveText('Transcribing…');
      await expectNoViolations(page);
      await release(page, 'cleanup');
    });

    test('finished', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record', 'Done');
      await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('failed', async ({ page }) => {
      await open(page, { transcribe: 'fail' });
      await press(page, 'New', 'Record', 'Done');
      await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('the speech model downloading', async ({ page }) => {
      await open(page, { model: 'download', hold: 'download' });
      await press(page, 'New');
      await expect(page.getByRole('progressbar')).toBeVisible();
      await expectNoViolations(page);
      await release(page, 'download');
    });

    test('the speech model download failed', async ({ page }) => {
      await open(page, { model: 'fail' });
      await expect(page.getByRole('button', { name: 'Retry download' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('recording, with the waveform moving', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record');
      await expect(page.locator('.wave')).toHaveAttribute('data-count', /^[1-9]/);
      await expectNoViolations(page);
    });

    test('paused, with the waveform frozen, and reduced motion', async ({ page }) => {
      await page.emulateMedia({ reducedMotion: 'reduce' });
      await open(page);
      await press(page, 'New', 'Record');
      await expect(page.locator('.level')).toBeVisible();
      await press(page, 'Pause');
      await expectNoViolations(page);
    });

    test('the library open and empty', async ({ page }) => {
      await open(page);
      await expect(page.getByRole('navigation', { name: 'Recordings' })).toContainText('No recordings yet.');
      await expectNoViolations(page);
    });

    test('the library open with entries', async ({ page }) => {
      await open(page, { library: '3' });
      await expect(page.locator('#library li')).toHaveCount(3);
      await expectNoViolations(page);
    });

    test('the library with two day groups', async ({ page }) => {
      await page.clock.setFixedTime(Date.UTC(2026, 9, 28, 12));
      await open(page, { library: '3' });
      await press(page, 'New', 'Record', 'Done');
      await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/^This is part 1 of the dictation\./);
      await expect(page.locator('#library .day')).toHaveText(['Today', 'Oct 26']);
      await expectNoViolations(page);
    });

    test('the library grouped by day at the narrowest width', async ({ page }) => {
      await page.clock.setFixedTime(Date.UTC(2026, 9, 27, 12));
      await open(page, { library: '3', libraryWidth: '110' });
      await page.locator('#library li').nth(1).getByRole('button').click();
      await expectNoViolations(page);
    });

    test('the drawer handle focused', async ({ page }) => {
      await open(page, { library: '3' });
      await page.locator('.library-handle').focus();
      await expectNoViolations(page);
    });

    test('a library entry selected', async ({ page }) => {
      await open(page, { library: '3' });
      await page.locator('#library li').nth(1).getByRole('button').click();
      await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue('This is preloaded recording number 2.');
      await expectNoViolations(page);
    });

    test('the settings modal open', async ({ page }) => {
      await open(page, { library: '3' });
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await expect(page.getByRole('switch')).toHaveCount(2);
      await expect(page.getByRole('switch', { name: 'Enable post-processing with local AI (Ollama)' })).toBeVisible();
      await expect(page.getByText('Version 0.0.0-mock')).toBeVisible();
      await expectNoViolations(page);
    });

    test('the settings modal with the text editor failing', async ({ page }) => {
      await open(page, { fail: 'vocabulary_open' });
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await page.getByRole('button', { name: 'Open in Text Editor' }).click();
      await expect(page.getByRole('dialog', { name: 'Settings' }).getByRole('alert')).toHaveText(
        'Could not open the vocabulary file: no text editor.',
      );
      await expectNoViolations(page);
    });

    test('the settings modal listing the models', async ({ page }) => {
      await open(page, { hold: 'ollama_models' });
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await expect(page.getByRole('combobox', { name: 'Title model' })).toBeDisabled();
      await expectNoViolations(page);
    });

    test('the settings modal with the models listed', async ({ page }) => {
      await open(page);
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await expect(page.getByRole('combobox', { name: 'Title model' })).toBeEnabled();
      await expectNoViolations(page);
    });

    test('the settings modal with Ollama unavailable', async ({ page }) => {
      await open(page, { ollama: 'down' });
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await expect(page.getByText('Local AI needs Ollama. Start Ollama, then reopen Settings.')).toBeVisible();
      await expectNoViolations(page);
    });

    test('the settings modal over a recording', async ({ page }) => {
      await open(page);
      await press(page, 'New', 'Record');
      await page.getByRole('button', { name: 'Settings', exact: true }).click();
      await expect(page.getByRole('switch')).toHaveCount(2);
      await expectNoViolations(page);
    });

    test('the microphone control in ready', async ({ page }) => {
      await open(page);
      await press(page, 'New');
      await expect(page.getByRole('button', { name: 'Microphone: Jabra Evolve 65' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('the microphone menu open', async ({ page }) => {
      await open(page);
      await press(page, 'New');
      await page.getByRole('button', { name: /^Microphone/ }).click();
      await expect(page.getByRole('menuitemradio', { name: 'AirPods Pro' })).toBeVisible();
      await expectNoViolations(page);
    });

    test('the microphone menu open with the listing failed', async ({ page }) => {
      await open(page, { fail: 'input_devices' });
      await press(page, 'New');
      await page.getByRole('button', { name: /^Microphone/ }).click();
      await expect(page.getByRole('alert')).toHaveText('Mock failure: input_devices');
      await expectNoViolations(page);
    });

    test('recording with the fallback notice', async ({ page }) => {
      await open(page, { inputDevice: 'Gone Mic' });
      await press(page, 'New', 'Record');
      await expect(status(page)).toContainText('Gone Mic is not available');
      await expectNoViolations(page);
    });

    test('session unreadable', async ({ page }) => {
      await open(page, { fail: 'dictation_state' });
      await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
      await expectNoViolations(page);
    });
  });
}
