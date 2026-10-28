import { expect, test } from '@playwright/test';

import {
  allowed,
  BAR_PX,
  barCount,
  GAP_PX,
  MAX_BARS,
  bars,
  downloadText,
  formatBytes,
  formatElapsed,
  type KeyPress,
  LEGAL,
  parts,
  percent,
  pushLevel,
  shortcut,
  SHORTCUTS,
  statusText,
} from '../../src/lib/format';
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
  download: null,
  libraryRevision: 0,
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
    [{ phase: 'transcribing', pending: 0 }, 'Transcribing…'],
    [{ phase: 'finished', notice: 'Copied to the clipboard.' }, 'Copied to the clipboard.'],
    [{ phase: 'finished', notice: null }, 'The transcript is ready.'],
    [{ phase: 'failed', error: 'Part 1 of 1 could not be transcribed.' }, 'The transcript is not ready.'],
  ];
  for (const [patch, want] of cases) expect(statusText({ ...base, ...patch }), JSON.stringify(patch)).toBe(want);
});

test('the bar geometry is 2 px bars 2 px apart, at most 512 levels', () => {
  expect([BAR_PX, GAP_PX, MAX_BARS]).toEqual([2, 2, 512]);
});

test('barCount is how many 4 px slots fit the width', () => {
  const cases: [number, number][] = [
    [0, 0],
    [1, 0],
    [2, 1],
    [6, 2],
    [488, 122],
    [568, 142],
    [928, 232],
    [100000, 512],
    [-10, 0],
    [NaN, 0],
    [Infinity, 0],
  ];
  for (const [width, want] of cases) expect(barCount(width), String(width)).toBe(want);
});

test('the waveform keeps the newest MAX_BARS levels, newest last, clamped to 0..1', () => {
  expect(pushLevel([], 0.5)).toEqual([0.5]);
  expect(pushLevel([0.1, 0.2], 0.3)).toEqual([0.1, 0.2, 0.3]);
  expect(pushLevel([], 1.5)).toEqual([1]);
  expect(pushLevel([], -0.2)).toEqual([0]);
  const full = Array.from({ length: MAX_BARS }, (_, i) => i / 1000);
  const next = pushLevel(full, 0.9);
  expect(next.length).toBe(MAX_BARS);
  expect(next[0]).toBe(0.001);
  expect(next.at(-1)).toBe(0.9);
});

test('the bars are the newest count levels right-aligned, the empty slots at 0', () => {
  expect(bars([], 5)).toEqual([0, 0, 0, 0, 0]);
  expect(bars([0.2, 0.7], 4)).toEqual([0, 0, 0.2, 0.7]);
  expect(bars([0, 1, 2, 3, 4, 5], 3)).toEqual([3, 4, 5]);
  expect(bars([0.2, 0.7], 0)).toEqual([]);
  expect(bars([0.2, 0.7], -1)).toEqual([]);
});

const press = (key: string, mods: Partial<KeyPress> = {}): KeyPress => ({
  key,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});

test('Space records, pauses and resumes; Return finishes; each only where the core allows it', () => {
  const space = press(' ');
  const enter = press('Enter');
  const want: Record<Phase, [string | null, string | null]> = {
    idle: [null, null],
    ready: ['record', null],
    recording: ['pause', 'done'],
    paused: ['resume', 'done'],
    transcribing: [null, null],
    finished: [null, null],
    failed: [null, null],
  };
  for (const phase of Object.keys(want) as Phase[]) {
    expect([shortcut(space, phase, false), shortcut(enter, phase, false)], phase).toEqual(want[phase]);
  }
});

test('Space and Return type in a text field, and only bare keys count', () => {
  expect(shortcut(press(' '), 'ready', true)).toBeNull();
  expect(shortcut(press('Enter'), 'recording', true)).toBeNull();
  for (const mod of ['metaKey', 'ctrlKey', 'altKey', 'shiftKey'] as const) {
    expect(shortcut(press(' ', { [mod]: true }), 'ready', false), mod).toBeNull();
    expect(shortcut(press('Enter', { [mod]: true }), 'paused', false), mod).toBeNull();
  }
  expect(shortcut(press('a'), 'ready', false)).toBeNull();
  expect(shortcut(press('n'), 'idle', false)).toBeNull();
});

test('Command-N is New wherever New is allowed, even in a text field', () => {
  const cmdN = press('n', { metaKey: true });
  expect(shortcut(cmdN, 'idle', false)).toBe('new');
  expect(shortcut(cmdN, 'finished', true)).toBe('new');
  expect(shortcut(press('N', { metaKey: true }), 'failed', false)).toBe('new');
  expect(shortcut(cmdN, 'recording', false)).toBeNull();
  expect(shortcut(press('n', { metaKey: true, shiftKey: true }), 'idle', false)).toBeNull();
  expect(shortcut(press('n', { ctrlKey: true }), 'idle', false)).toBeNull();
  expect(shortcut(press('m', { metaKey: true }), 'idle', false)).toBeNull();
});

test('each shortcut names its key for assistive tech and shows a hint', () => {
  expect(SHORTCUTS).toEqual({
    new: { aria: 'Meta+N', hint: '⌘N' },
    record: { aria: 'Space', hint: 'Space' },
    pause: { aria: 'Space', hint: 'Space' },
    resume: { aria: 'Space', hint: 'Space' },
    done: { aria: 'Enter', hint: '⏎' },
  });
});

test('sizes read like Finder: powers of 1000, one decimal below 10', () => {
  const cases: [number, string][] = [
    [0, '0 B'],
    [999, '999 B'],
    [1_000, '1 kB'],
    [1_500, '1.5 kB'],
    [12_345, '12 kB'],
    [999_600_000, '1 GB'],
    [680_000_000, '680 MB'],
    [1_624_555_275, '1.6 GB'],
    [9_960_000_000, '10 GB'],
    [5e15, '5000 TB'],
  ];
  for (const [bytes, want] of cases) expect(formatBytes(bytes), String(bytes)).toBe(want);
});

test('the download shows whole percent and bytes, and why Record waits', () => {
  expect(percent({ receivedBytes: 0, totalBytes: 100 })).toBe(0);
  expect(percent({ receivedBytes: 999, totalBytes: 1000 })).toBe(99);
  expect(percent({ receivedBytes: 1000, totalBytes: 1000 })).toBe(100);
  expect(percent({ receivedBytes: 2000, totalBytes: 1000 })).toBe(100);
  expect(percent({ receivedBytes: 5, totalBytes: 0 })).toBe(0);
  expect(downloadText({ receivedBytes: 680_000_000, totalBytes: 1_624_555_275 })).toBe(
    'Downloading the speech model, needed once: 41% (680 MB of 1.6 GB). Record works when it is done.',
  );
});
