import { expect, test } from './fixtures';

import { button, copied, hold, open, press, release, status } from './helpers';

// The dictation flow end to end, against the mock bridge: New, Record, Pause,
// Resume, Done, the transcript on the clipboard and in an editable box.

const PART_1 = 'This is part 1 of the dictation.';
const PART_2 = 'This is part 2 of the dictation.';
const BUTTONS = ['New', 'Record', 'Pause', 'Resume', 'Done', 'Discard'];

async function enabled(page: import('@playwright/test').Page): Promise<string[]> {
  const names: string[] = [];
  for (const name of BUTTONS) if ((await button(page, name).count()) > 0 && (await button(page, name).isEnabled())) names.push(name);
  return names;
}

test('a fresh window offers only New', async ({ page }) => {
  await open(page);
  await expect(status(page)).toHaveText('Click New to start a dictation.');
  expect(await enabled(page)).toEqual(['New']);
  await expect(page.getByTitle('Recorded time')).toHaveCount(0);
});

test('record, pause, resume, done: the transcript is copied and shown in paragraphs', async ({ page }) => {
  // Running the fake clock through 105 s of animated level meter takes ~20 s
  // under coverage, past the 30 s default once six workers share the host.
  test.slow();
  await page.clock.install();
  await open(page);
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  expect(await enabled(page)).toEqual(['Record', 'Discard']);
  await expect(page.getByTitle('Recorded time')).toHaveText('Recorded 0:00');

  await press(page, 'Record');
  await expect(status(page)).toHaveText('Recording. Pause to think, Done when you have said it all.');
  expect(await enabled(page)).toEqual(['Pause', 'Done', 'Discard']);
  await page.clock.runFor(5_000);
  await expect(page.getByTitle('Recorded time')).toHaveText('Recorded 0:05');

  await press(page, 'Pause');
  await expect(status(page)).toHaveText('Paused after 1 part. Resume when you are ready.');
  expect(await enabled(page)).toEqual(['Resume', 'Done', 'Discard']);
  await page.clock.runFor(30_000);
  await expect(page.getByTitle('Recorded time')).toHaveText('Recorded 0:05');

  await press(page, 'Resume');
  await page.clock.runFor(70_000);
  await expect(page.getByTitle('Recorded time')).toHaveText('Recorded 1:15');
  await press(page, 'Done');

  await expect(status(page)).toHaveText('Copied to the clipboard.');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await expect(transcript).toHaveValue(`${PART_1}\n\n${PART_2}`);
  expect(await copied(page)).toEqual([`${PART_1}\n\n${PART_2}`]);
  expect(await enabled(page)).toEqual(['New', 'Discard']);
});

test('segments transcribe while paused, and Done waits for the last one', async ({ page }) => {
  await open(page);
  await hold(page, 'transcribe');
  await press(page, 'New', 'Record', 'Pause');
  await expect(status(page)).toHaveText('Paused. Transcribing 1 part while you think.');
  await press(page, 'Resume', 'Done');
  await expect(status(page)).toHaveText('Transcribing the last 2 parts…');
  await expect(page.locator('.controls button')).toHaveCount(0);
  await expect(page.locator('.controls')).toHaveCount(1);
  expect(await enabled(page)).toEqual([]);
  expect(await copied(page)).toEqual([]);
  await release(page, 'transcribe');
  await expect(status(page)).toHaveText('Copied to the clipboard.');
});

test('an edited transcript is copied again with Copy', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  const transcript = page.getByRole('textbox', { name: 'Transcript' });
  await transcript.fill('My own words.');
  await press(page, 'Copy');
  expect(await copied(page)).toEqual([PART_1, 'My own words.']);
  await expect(status(page)).toHaveText('Copied to the clipboard.');
  await transcript.fill('');
  await expect(button(page, 'Copy')).toBeDisabled();
});

test('New after a transcript starts an empty session', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done', 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveCount(0);
  await expect(page.getByTitle('Recorded time')).toHaveText('Recorded 0:00');
});

test('Discard throws a recording away', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Pause', 'Resume', 'Discard');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await press(page, 'Record', 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(PART_1);
});

test('buttons wait while an action is in flight', async ({ page }) => {
  await open(page);
  await press(page, 'New');
  await hold(page, 'dictation_act');
  await button(page, 'Record').click();
  expect(await enabled(page)).toEqual([]);
  await expect(button(page, 'Record')).toBeDisabled();
  await expect(button(page, 'Discard')).toBeDisabled();
  await release(page, 'dictation_act');
  await expect(button(page, 'Pause')).toBeEnabled();
});

test('a failed transcription is an error with Retry, never an empty transcript', async ({ page }) => {
  await open(page, { transcribe: 'fail' });
  await press(page, 'New', 'Record', 'Done');
  await expect(page.getByRole('alert')).toHaveText(
    'Part 1 of 1 could not be transcribed: model exploded. Retry to try again.',
  );
  await expect(status(page)).toHaveText('The transcript is not ready.');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveCount(0);
  await press(page, 'Retry');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(PART_1);
  await expect(button(page, 'Retry')).toHaveCount(0);
});

test('a session left unfinished at the last launch is offered for Retry', async ({ page }) => {
  await open(page, { leftover: '2' });
  await expect(page.getByRole('alert')).toHaveText(
    'The last session was not transcribed. Retry to transcribe its 2 recorded parts, or start a new session.',
  );
  await press(page, 'Retry');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(`${PART_1}\n\n${PART_2}`);
});

test('silence says nothing was heard and leaves the clipboard alone', async ({ page }) => {
  await open(page, { mic: 'silent' });
  await press(page, 'New', 'Record', 'Done');
  await expect(status(page)).toContainText('No speech was heard.');
  expect(await copied(page)).toEqual([]);
  await expect(button(page, 'Copy')).toBeDisabled();
});

test('a clipboard that cannot be written is reported, and Copy says so too', async ({ page }) => {
  await open(page, { clipboard: 'fail' });
  await press(page, 'New', 'Record', 'Done');
  await expect(status(page)).toHaveText(
    'Could not copy to the clipboard: pasteboard unavailable. Use Copy to try again.',
  );
  await press(page, 'Copy');
  await expect(page.getByRole('alert')).toHaveText('Could not copy to the clipboard: pasteboard unavailable');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(PART_1);
});

test('a refused action is shown and cleared by the next one', async ({ page }) => {
  await open(page, { fail: 'pause' });
  await press(page, 'New', 'Record', 'Pause');
  await expect(page.getByRole('alert')).toHaveText('Mock failure: pause');
  await expect(status(page)).toContainText('Recording.');
  await press(page, 'Done');
  await expect(page.getByRole('alert')).toHaveCount(0);
});

test('a session that cannot be read offers Try again', async ({ page }) => {
  await open(page, { fail: 'dictation_state' });
  await expect(page.getByRole('alert')).toHaveText('Could not read the session: Mock failure: dictation_state');
  await expect(button(page, 'New')).toBeDisabled();
  await hold(page, 'dictation_state');
  await button(page, 'Try again').click();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await release(page, 'dictation_state');
  await expect(page.getByRole('alert')).toHaveText('Could not read the session: Mock failure: dictation_state');
});

test('the window still works when the version cannot be read', async ({ page }) => {
  await open(page, { fail: 'app_version' });
  await press(page, 'New');
  await expect(status(page)).toHaveText('Click Record and start talking.');
  await button(page, 'Settings').click();
  await expect(page.getByRole('textbox', { name: 'Vocabulary file' })).toBeVisible();
  await expect(page.getByText(/^Version/)).toHaveCount(0);
});

test('buttons the phase does not offer are absent, not disabled', async ({ page }) => {
  await open(page);
  const absent = async (names: string[]): Promise<void> => {
    for (const name of names) await expect(button(page, name), name).toHaveCount(0);
  };
  await press(page, 'New');
  await absent(['Pause', 'Resume', 'Done']);
  await press(page, 'Record');
  await absent(['Record', 'Resume']);
  await press(page, 'Pause');
  await absent(['Record', 'Pause']);
  await press(page, 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  await absent(['Record', 'Pause', 'Resume', 'Done']);
  await open(page, { transcribe: 'fail' });
  await press(page, 'New', 'Record', 'Done');
  await expect(button(page, 'Retry')).toBeVisible();
  await absent(['Record', 'Pause', 'Resume', 'Done']);
});

test('each phase lists its controls in order, with the primary first and Discard quiet', async ({ page }) => {
  const labels = (): Promise<(string | undefined)[]> =>
    page.locator('.controls button').evaluateAll((els) => els.map((el) => el.childNodes[0]?.textContent?.trim()));
  await open(page);
  await press(page, 'New');
  expect(await labels()).toEqual(['Record', 'Discard']);
  await expect(button(page, 'Record')).toHaveClass(/primary/);
  await expect(button(page, 'Discard')).toHaveClass(/quiet/);
  await press(page, 'Record');
  expect(await labels()).toEqual(['Pause', 'Done', 'Discard']);
  await expect(button(page, 'Pause')).toHaveClass(/primary/);
  await expect(button(page, 'Done')).not.toHaveClass(/primary/);
  await press(page, 'Pause');
  expect(await labels()).toEqual(['Resume', 'Done', 'Discard']);
  await expect(button(page, 'Resume')).toHaveClass(/primary/);
  await press(page, 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  expect(await labels()).toEqual(['Discard']);
  await open(page, { transcribe: 'fail' });
  await press(page, 'New', 'Record', 'Done');
  await expect(button(page, 'Retry')).toBeVisible();
  expect(await labels()).toEqual(['Retry', 'Discard']);
  await expect(button(page, 'Retry')).toHaveClass(/primary/);
});

test('New is the primary button only in a fresh window', async ({ page }) => {
  await open(page, { library: '1' });
  await expect(button(page, 'New')).toHaveClass(/primary/);
  await page.locator('#library li').first().getByRole('button').click();
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toBeVisible();
  await expect(button(page, 'New')).not.toHaveClass(/primary/);
  await press(page, 'New');
  await expect(button(page, 'New')).not.toHaveClass(/primary/);
});

async function copiedAt60s(page: import('@playwright/test').Page): Promise<void> {
  await page.clock.install({ time: 0 });
  await open(page, { libraryOpen: '0' });
  await page.clock.pauseAt(60_000);
  await press(page, 'New', 'Record', 'Done');
  await press(page, 'Copy');
  await expect(page.locator('button.copy.copied')).toBeVisible();
}

test('the check shows for 2 seconds, then Copy returns', async ({ page }) => {
  await copiedAt60s(page);
  await page.clock.runFor(1999);
  await expect(page.locator('button.copy.copied')).toBeVisible();
  await page.clock.runFor(1);
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
  await expect(button(page, 'Copy')).toBeVisible();
});

test('a second click restarts the 2 seconds', async ({ page }) => {
  await copiedAt60s(page);
  await page.clock.runFor(1000);
  await press(page, 'Copy');
  await expect(page.locator('button.copy.copied')).toBeVisible();
  await page.clock.runFor(1999);
  await expect(page.locator('button.copy.copied')).toBeVisible();
  await page.clock.runFor(1);
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});

test('an edit does not end the check', async ({ page }) => {
  await copiedAt60s(page);
  await page.getByRole('textbox', { name: 'Transcript' }).fill('');
  await expect(page.locator('button.copy.copied')).toBeVisible();
  await expect(button(page, 'Copy')).toBeDisabled();
  await page.clock.runFor(2000);
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});

test('the automatic copy shows no check', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  await expect(page.getByRole('textbox', { name: 'Transcript' })).toHaveValue(/part 1 of the dictation\.$/);
  await expect(status(page)).toHaveText('Copied to the clipboard.');
  await expect(button(page, 'Copy')).toBeEnabled();
  expect(await copied(page)).toHaveLength(1);
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});

test('a failed copy shows no check', async ({ page }) => {
  await open(page, { clipboard: 'fail' });
  await press(page, 'New', 'Record', 'Done', 'Copy');
  await expect(page.getByRole('alert')).toContainText('Could not copy to the clipboard: pasteboard unavailable');
  await expect(page.locator('button.copy.copied')).toHaveCount(0);
});

test('Copy has a title and works from the keyboard', async ({ page }) => {
  await open(page);
  await press(page, 'New', 'Record', 'Done');
  await expect(button(page, 'Copy')).toBeEnabled();
  await expect(button(page, 'Copy')).toHaveAttribute('title', 'Copy');
  await page.getByRole('textbox', { name: 'Transcript' }).focus();
  await page.keyboard.press('Tab');
  await expect(button(page, 'Copy')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.locator('button.copy.copied')).toBeVisible();
});
