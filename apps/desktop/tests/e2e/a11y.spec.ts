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

for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`${colorScheme} mode`, () => {
    test.use({ colorScheme });

    test('idle', async ({ page }) => {
      await open(page);
      await expect(status(page)).toHaveText('Click New to start a dictation.');
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

    test('session unreadable', async ({ page }) => {
      await open(page, { fail: 'dictation_state' });
      await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
      await expectNoViolations(page);
    });
  });
}
