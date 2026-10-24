import { expect, test } from '@playwright/test';

import { allowed, formatElapsed, LEGAL, parts, statusText } from '../../src/lib/format';
import type { Phase, Snapshot } from '../../src/lib/types';

const base: Snapshot = {
  revision: 1,
  phase: 'idle',
  recordedMs: 0,
  segments: 0,
  pending: 0,
  text: '',
  error: null,
  notice: null,
  modelError: null,
};

test('recorded time reads m:ss, then h:mm:ss, rounded down', () => {
  const cases: [number, string][] = [
    [0, '0:00'],
    [999, '0:00'],
    [1_000, '0:01'],
    [59_999, '0:59'],
    [60_000, '1:00'],
    [75_500, '1:15'],
    [3_599_999, '59:59'],
    [3_600_000, '1:00:00'],
    [3_725_000, '1:02:05'],
    [-5, '0:00'],
  ];
  for (const [ms, want] of cases) expect(formatElapsed(ms), String(ms)).toBe(want);
});

test('parts are counted in words', () => {
  expect(parts(1)).toBe('1 part');
  expect(parts(0)).toBe('0 parts');
  expect(parts(3)).toBe('3 parts');
});

test('the buttons follow the legal actions per phase', () => {
  expect(allowed('idle', 'new')).toBe(true);
  expect(allowed('idle', 'record')).toBe(false);
  expect(allowed('transcribing', 'discard')).toBe(false);
  for (const phase of Object.keys(LEGAL) as Phase[]) {
    for (const action of LEGAL[phase]) expect(allowed(phase, action), `${phase} ${action}`).toBe(true);
  }
});

test('the status line says what to do next in every phase', () => {
  const cases: [Partial<Snapshot>, string][] = [
    [{ phase: 'idle' }, 'Click New to start a dictation.'],
    [{ phase: 'ready' }, 'Click Record and start talking.'],
    [{ phase: 'recording' }, 'Recording. Pause to think, Done when you have said it all.'],
    [{ phase: 'paused', segments: 2, pending: 1 }, 'Paused. Transcribing 1 part while you think.'],
    [{ phase: 'paused', segments: 2, pending: 0 }, 'Paused after 2 parts. Resume when you are ready.'],
    [{ phase: 'transcribing', pending: 2 }, 'Transcribing the last 2 parts…'],
    [{ phase: 'finished', notice: 'Copied to the clipboard.' }, 'Copied to the clipboard.'],
    [{ phase: 'finished', notice: null }, 'The transcript is ready.'],
    [{ phase: 'failed', error: 'Part 1 of 1 could not be transcribed.' }, 'The transcript is not ready.'],
  ];
  for (const [patch, want] of cases) expect(statusText({ ...base, ...patch }), JSON.stringify(patch)).toBe(want);
});
