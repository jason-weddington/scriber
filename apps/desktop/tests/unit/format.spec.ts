import { expect, test } from '@playwright/test';

import {
  activeMicrophone,
  allowed,
  defaultMicrophoneLabel,
  BAR_PX,
  barCount,
  GAP_PX,
  MAX_BARS,
  bars,
  clampLibraryWidth,
  dayLabel,
  groupByDay,
  rowSummary,
  timeLabel,
  downloadText,
  fileName,
  controlsFor,
  formatBytes,
  formatElapsed,
  type KeyPress,
  LEGAL,
  opensSettings,
  parts,
  percent,
  pushLevel,
  shortcut,
  SETTINGS_SHORTCUT,
  SHORTCUTS,
  statusText,
  statusLine,
  statusAlert,
} from '../../src/lib/format';
import type { InputDevices, Phase, Snapshot } from '../../src/lib/types';

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
  microphone: null,
  learned: [],
  partial: '',
  rawText: null,
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

test('the recorder row offers exactly these controls in each phase', () => {
  const record = { action: 'record', label: 'Record', rank: 'primary' };
  const pause = { action: 'pause', label: 'Pause', rank: 'primary' };
  const resume = { action: 'resume', label: 'Resume', rank: 'primary' };
  const done = { action: 'done', label: 'Done', rank: 'secondary' };
  const retry = { action: 'retry', label: 'Retry', rank: 'primary' };
  const discard = { action: 'discard', label: 'Discard', rank: 'quiet' };
  const table: Record<Phase, object[]> = {
    idle: [],
    ready: [record, discard],
    recording: [pause, done, discard],
    paused: [resume, done, discard],
    transcribing: [],
    finished: [discard],
    failed: [retry, discard],
  };
  for (const phase of Object.keys(table) as Phase[]) {
    expect(controlsFor(phase), phase).toEqual(table[phase]);
    const offered = controlsFor(phase).map((c) => c.action).sort();
    const legal = LEGAL[phase].filter((a) => a !== 'new' && a !== 'copy' && a !== 'accept' && a !== 'undoLearn').sort();
    expect(offered, phase).toEqual(legal);
  }
});

test('the status line says what to do next in every phase', () => {
  const cases: [Partial<Snapshot>, string][] = [
    [{ phase: 'idle' }, 'Click New to start a dictation.'],
    [{ phase: 'ready' }, 'Click Record and start talking.'],
    [{ phase: 'recording' }, 'Recording. Pause to think, Done when you have said it all.'],
    [{ phase: 'recording', notice: 'Gone Mic is not available.' }, 'Gone Mic is not available.'],
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

test('the bar geometry is 4 px bars 2 px apart, at most 512 levels', () => {
  expect([BAR_PX, GAP_PX, MAX_BARS]).toEqual([4, 2, 512]);
});

test('barCount is how many 6 px slots fit the width', () => {
  const cases: [number, number][] = [
    [0, 0],
    [1, 0],
    [3, 0],
    [4, 1],
    [9, 1],
    [10, 2],
    [488, 81],
    [568, 95],
    [928, 155],
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

// ---- the library drawer -------------------------------------------------------------

test('clampLibraryWidth rounds to whole px within 110 to 440', () => {
  for (const [px, want] of [[109.4, 110], [110, 110], [220.5, 221], [440, 440], [441, 440]] as const) {
    expect(clampLibraryWidth(px), String(px)).toBe(want);
  }
});

test('dayLabel names the day from the title and the local date', () => {
  const now = new Date(2026, 9, 30, 12, 0);
  const cases: [string, string][] = [
    ['2026-10-30 12:00:00', 'Today'],
    ['2026-10-30 00:00:00', 'Today'],
    ['2026-10-30 23:59:59', 'Today'],
    ['2026-10-29 18:00:00', 'Yesterday'],
    ['2026-10-28 18:00:00', 'Oct 28'],
    ['2025-12-31 09:00:00', 'Dec 31, 2025'],
    ['2026-10-31 09:00:00', 'Oct 31'],
    ['2026-13-45 00:00:00', 'Feb 14, 2027'],
    ['not a title', 'Earlier'],
    ['2026-10-30 12:00', 'Earlier'],
    ['', 'Earlier'],
  ];
  for (const [title, want] of cases) expect(dayLabel(title, now), title).toBe(want);
  expect(dayLabel('2026-10-31 08:00:00', new Date(2026, 10, 1, 9, 0)), 'across a month').toBe('Yesterday');
  expect(dayLabel('2026-12-31 08:00:00', new Date(2027, 0, 1, 9, 0)), 'across a year').toBe('Yesterday');
});

test('timeLabel is a 12-hour clock with an ASCII space', () => {
  const cases: [string, string][] = [
    ['2026-10-26 09:44:12', '9:44 AM'],
    ['2026-10-26 00:05:00', '12:05 AM'],
    ['2026-10-26 11:59:59', '11:59 AM'],
    ['2026-10-26 12:00:00', '12:00 PM'],
    ['2026-10-26 13:07:00', '1:07 PM'],
    ['2026-10-26 23:59:00', '11:59 PM'],
    ['2026-10-26 25:00:00', '1:00 PM'],
    ['whenever', 'whenever'],
  ];
  for (const [title, want] of cases) expect(timeLabel(title), title).toBe(want);
});

test('rowSummary falls back for a blank summary', () => {
  expect(rowSummary('')).toBe('Untitled recording');
  expect(rowSummary('   ')).toBe('Untitled recording');
  expect(rowSummary('A note.')).toBe('A note.');
});

test('groupByDay groups by label in order of first appearance', () => {
  const now = new Date(2026, 9, 30, 12, 0);
  const e = (id: string, title: string) => ({ id, title, summary: id });
  expect(groupByDay([], now)).toEqual([]);
  const a = e('a', '2026-10-30 10:00:00');
  const b = e('b', '2026-10-29 10:00:00');
  const c = e('c', '2026-10-30 09:00:00');
  const d = e('d', 'odd');
  const f = e('f', 'also odd');
  expect(groupByDay([a, b, c, d, f], now)).toEqual([
    { label: 'Today', entries: [a, c] },
    { label: 'Yesterday', entries: [b] },
    { label: 'Earlier', entries: [d, f] },
  ]);
});

test('the microphone named is the one open while recording, else the one the next start opens', () => {
  const inputs: InputDevices = { defaultDevice: 'Jabra Evolve 65', others: ['AirPods Pro', 'MacBook Pro Microphone'] };
  const live: Snapshot = { ...base, phase: 'recording', microphone: 'Jabra Evolve 65' };
  expect(activeMicrophone(live, inputs, 'AirPods Pro'), 'recording: what is open').toBe('Jabra Evolve 65');
  expect(activeMicrophone({ ...live, microphone: null }, inputs, 'AirPods Pro'), 'recording, none known').toBe('AirPods Pro');
  const paused: Snapshot = { ...live, phase: 'paused' };
  expect(activeMicrophone(paused, inputs, 'AirPods Pro'), 'paused: the next choice').toBe('AirPods Pro');
  expect(activeMicrophone(paused, inputs, ''), 'paused: the default').toBe('Jabra Evolve 65');
  expect(activeMicrophone({ ...base, phase: 'ready' }, inputs, 'Gone Mic'), 'a saved name not listed').toBe('Jabra Evolve 65');
  expect(activeMicrophone({ ...base, phase: 'ready' }, null, 'AirPods Pro'), 'no list yet').toBeNull();
  expect(activeMicrophone({ ...base, phase: 'ready' }, { defaultDevice: null, others: [] }, ''), 'no device').toBeNull();
});

test('the menu names the Mac default with its device', () => {
  expect(defaultMicrophoneLabel('Jabra Evolve 65')).toBe('Mac default (Jabra Evolve 65)');
  expect(defaultMicrophoneLabel(null)).toBe('Mac default');
});

test('fileName shows the last path component, never blank', () => {
  expect(fileName('/mock/data/vocabulary.txt')).toBe('vocabulary.txt');
  expect(fileName('terms.txt')).toBe('terms.txt');
  expect(fileName('')).toBe('');
  expect(fileName('/')).toBe('/');
  expect(fileName('/x/dir/')).toBe('/x/dir/');
  expect(fileName('/a b/my terms.txt')).toBe('my terms.txt');
  expect(fileName('/x/a\\b.txt')).toBe('a\\b.txt');
});

test('⌘, opens Settings, and only ⌘, alone', () => {
  expect(opensSettings(press(',', { metaKey: true }))).toBe(true);
  expect(opensSettings(press(','))).toBe(false);
  expect(opensSettings(press(',', { ctrlKey: true }))).toBe(false);
  expect(opensSettings(press('.', { metaKey: true }))).toBe(false);
  expect(opensSettings(press('n', { metaKey: true }))).toBe(false);
  for (const mod of ['ctrlKey', 'altKey', 'shiftKey'] as const) {
    expect(opensSettings(press(',', { metaKey: true, [mod]: true })), mod).toBe(false);
  }
});

test('⌘, is no dictation shortcut in any phase', () => {
  const phases: Phase[] = ['idle', 'ready', 'recording', 'paused', 'transcribing', 'finished', 'failed'];
  for (const phase of phases) {
    for (const inText of [true, false]) {
      expect(shortcut(press(',', { metaKey: true }), phase, inText), `${phase} ${String(inText)}`).toBeNull();
    }
  }
});

test('the Settings shortcut is named and shown as ⌘,', () => {
  expect(SETTINGS_SHORTCUT).toEqual({ aria: 'Meta+Comma', hint: '⌘,' });
});

test('the status line adds what a Copy learned, only when finished', () => {
  const fin: Snapshot = { ...base, phase: 'finished', notice: 'Copied to the clipboard.', learned: ['tightbeam', 'Scriber'] };
  expect(statusLine(fin)).toBe('Copied to the clipboard. Learned: tightbeam, Scriber.');
  expect(statusLine({ ...fin, learned: [] })).toBe('Copied to the clipboard.');
  const rec: Snapshot = { ...fin, phase: 'recording', notice: null };
  expect(statusLine(rec)).toBe(statusText(rec));
});

test('the status alert: action error, then failure, then model error', () => {
  const failed: Snapshot = { ...base, phase: 'failed', error: 'boom', modelError: 'no model' };
  expect(statusAlert(failed, 'act')).toBe('act');
  expect(statusAlert(failed, null)).toBe('boom');
  expect(statusAlert({ ...failed, error: null }, null)).toBe('no model');
  expect(statusAlert({ ...failed, phase: 'ready' }, null)).toBe('no model');
  expect(statusAlert({ ...base, modelError: 'no model' }, 'act')).toBe('act');
  expect(statusAlert(base, null)).toBeNull();
});
