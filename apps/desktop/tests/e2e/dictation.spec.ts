import { expect, test } from './fixtures';

import { button, copied, hold, open, press, release, status } from './helpers';

// The dictation flow end to end, against the mock bridge: New, Record, Pause,
// Resume, Done, the transcript on the clipboard and in an editable box.

const PART_1 = 'This is part 1 of the dictation.';
const PART_2 = 'This is part 2 of the dictation.';
const BUTTONS = ['New', 'Record', 'Pause', 'Resume', 'Done', 'Discard'];

async function enabled(page: import('@playwright/test').Page): Promise<string[]> {
  const names: string[] = [];
  for (const name of BUTTONS) if (await button(page, name).isEnabled()) names.push(name);
  return names;
}

test('a fresh window offers only New, and shows the mock version', async ({ page }) => {
  await open(page);
  await expect(status(page)).toHaveText('Click New to start a dictation.');
  expect(await enabled(page)).toEqual(['New']);
  await expect(page.getByText('Version 0.0.0-mock')).toBeVisible();
  await expect(page.getByTitle('Recorded time')).toHaveCount(0);
});

test('record, pause, resume, done: the transcript is copied and shown in paragraphs', async ({ page }) => {
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
  await release(page, 'dictation_act');
  await expect(button(page, 'Pause')).toBeEnabled();
});

test('a missing model is shown at once and Record stays off', async ({ page }) => {
  await open(page, { model: 'missing' });
  await expect(page.getByRole('alert')).toContainText('Model missing: run scripts/fetch-model.sh');
  await press(page, 'New');
  await expect(button(page, 'Record')).toBeDisabled();
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
  await expect(page.getByText(/^Version/)).toHaveCount(0);
});
