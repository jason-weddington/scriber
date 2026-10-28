import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

import { LEGAL } from '../../src/lib/format';
import {
  COPIED,
  DOWNLOAD_REASON,
  DOWNLOAD_STEP_MS,
  DOWNLOAD_STEPS,
  DOWNLOADING,
  downloadFailed,
  excerpt,
  heard,
  join,
  LEVEL_MS,
  leftover,
  Mock,
  MOCK_SUMMARY,
  MOCK_VOCABULARY,
  MOCK_VERSION,
  MODEL_BYTES,
  modelMissing,
  NO_DOWNLOAD,
  NOT_IN_LIBRARY,
  NOTHING_HEARD,
  partFailed,
  preloaded,
  refusal,
  syntheticLevel,
  title,
} from '../../src/lib/mock';
import type { Action, Phase, Snapshot } from '../../src/lib/types';

// The mock must answer the way scriber-core does (docs/design.md), or the UI
// is tested against behaviour the real app never produces. Three checks:
// the mock's JSON shapes match the Rust fixture that the core's serde tests
// pin; its rules and sentences match a transcription of the core's; and it
// identifies itself, so it can never pass for the real app.

const FIXTURE = resolve(process.cwd(), '../../crates/scriber-core/tests/fixtures/bridge-shapes.json');
const shapes = JSON.parse(readFileSync(FIXTURE, 'utf8')) as Record<string, Record<string, unknown>>;

const PHASES: Phase[] = ['idle', 'ready', 'recording', 'paused', 'transcribing', 'finished', 'failed'];
const ACTIONS: Action[] = ['new', 'record', 'pause', 'resume', 'done', 'discard', 'retry', 'copy'];

/** Each key, and the JSON types its value takes across the fixture's examples. */
function types(examples: Record<string, unknown>[]): Map<string, Set<string>> {
  const out = new Map<string, Set<string>>();
  for (const ex of examples) {
    for (const [k, v] of Object.entries(ex)) {
      const set = out.get(k) ?? new Set<string>();
      set.add(v === null ? 'null' : typeof v);
      out.set(k, set);
    }
  }
  return out;
}

// ---- the core's rules, transcribed -------------------------------------------
//
// From crates/scriber-core/src/dictation.rs and model.rs. If the core changes,
// this changes with it and the mock must follow.

const CORE_LEGAL: Record<Phase, Action[]> = {
  idle: ['new'],
  ready: ['record', 'discard'],
  recording: ['pause', 'done', 'discard'],
  paused: ['resume', 'done', 'discard'],
  transcribing: [],
  finished: ['new', 'discard', 'copy'],
  failed: ['new', 'discard', 'retry'],
};

const CORE_ACTION_NAMES: Record<Action, string> = {
  new: 'start a new session',
  record: 'record',
  pause: 'pause',
  resume: 'resume',
  done: 'finish',
  discard: 'discard',
  retry: 'retry',
  copy: 'copy',
  downloadModel: 'download the speech model',
};

/** A mock driven to `phase` (transcriptions held at 'transcribing'). */
async function at(phase: Phase): Promise<Mock> {
  const flaky = phase === 'failed' ? 'transcribe=fail&' : '';
  const mock = new Mock(`?${flaky}`);
  const steps: Record<Phase, Action[]> = {
    idle: [],
    ready: ['new'],
    recording: ['new', 'record'],
    paused: ['new', 'record', 'pause'],
    transcribing: ['new', 'record', 'done'],
    finished: ['new', 'record', 'done'],
    failed: ['new', 'record', 'done'],
  };
  if (phase === 'transcribing') mock.handle.hold('transcribe');
  for (const step of steps[phase]) await mock.act(step, '');
  await settled();
  expect(mock.snapshot().phase, `reached ${phase}`).toBe(phase);
  return mock;
}

/** Lets the mock's background transcriptions run. */
async function settled(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

test.describe('the mock speaks the Rust JSON shapes', () => {
  test('every snapshot field, with the types the fixture allows', async () => {
    const allowedTypes = types([shapes.snapshot ?? {}, shapes.snapshotFailed ?? {}, shapes.snapshotDownloading ?? {}]);
    const downloading = new Mock('?model=download');
    const failed = new Mock('?model=fail');
    await finished(failed);
    const snapshots = [downloading.snapshot(), failed.snapshot(), new Mock('?leftover=1').snapshot()];
    expect(downloading.snapshot().download).not.toBeNull();
    expect(failed.snapshot().modelError).not.toBeNull();
    for (const phase of PHASES) snapshots.push((await at(phase)).snapshot());
    for (const s of snapshots) {
      expect(Object.keys(s).sort(), `${s.phase} keys`).toEqual([...allowedTypes.keys()].sort());
      for (const [k, v] of Object.entries(s)) {
        expect(allowedTypes.get(k)?.has(v === null ? 'null' : typeof v), `${s.phase} ${k}`).toBe(true);
      }
      if (s.download !== null) {
        expect(Object.keys(s.download).sort()).toEqual(Object.keys(shapes.snapshotDownloading?.download ?? {}).sort());
      }
    }
  });

  test('a library entry and the settings have the fixture keys and types', async () => {
    const mock = new Mock('?library=2');
    const [entry] = await mock.libraryList();
    const settings = await mock.settingsGet();
    for (const [name, value] of [
      ['libraryEntry', entry],
      ['settings', settings],
    ] as const) {
      const allowedTypes = types([shapes[name] ?? {}]);
      expect(Object.keys(value ?? {}).sort(), `${name} keys`).toEqual([...allowedTypes.keys()].sort());
      for (const [k, v] of Object.entries(value ?? {})) {
        expect(allowedTypes.get(k)?.has(typeof v), `${name} ${k}`).toBe(true);
      }
    }
  });

  test('a level is a number from 0 to 1, as in the fixture', () => {
    expect(typeof shapes.level).toBe('number');
    const all = Array.from({ length: 200 }, (_, n) => syntheticLevel(n));
    for (const level of all) {
      expect(level).toBeGreaterThanOrEqual(0);
      expect(level).toBeLessThanOrEqual(1);
    }
    expect(Math.min(...all)).toBeGreaterThanOrEqual(0.1);
    expect(Math.max(...all)).toBeGreaterThan(0.8);
  });
});

/** Waits until the mock's download (if any) is over. */
async function finished(mock: Mock): Promise<void> {
  for (let i = 0; i < 100 && mock.snapshot().download !== null; i++) {
    await new Promise((resolve) => setTimeout(resolve, DOWNLOAD_STEP_MS));
  }
  expect(mock.snapshot().download, 'the download is over').toBeNull();
}

test('the legal actions are the core table, in the mock and in the buttons', async () => {
  expect(LEGAL).toEqual(CORE_LEGAL);
  for (const phase of PHASES) {
    for (const action of ACTIONS) {
      const mock = await at(phase);
      const before = mock.snapshot();
      const attempt = mock.act(action, 'edited');
      if (CORE_LEGAL[phase].includes(action)) {
        // One for the action, and one more when it archived a finished session into the library.
        const after = await attempt;
        expect(after.revision - before.revision, `${action} from ${phase}`).toBe(1 + after.libraryRevision - before.libraryRevision);
      } else {
        await expect(attempt, `${action} from ${phase}`).rejects.toThrow(
          `Cannot ${CORE_ACTION_NAMES[action]} while ${phase}.`,
        );
        expect(mock.snapshot(), `${action} from ${phase} changed nothing`).toEqual(before);
      }
    }
  }
});

test('the sentences are the core sentences', () => {
  expect(refusal('done', 'ready')).toBe('Cannot finish while ready.');
  expect(modelMissing('/m.bin')).toBe('The speech model is missing from /m.bin.');
  expect(downloadFailed('offline')).toBe('Could not download the speech model: offline.');
  expect(DOWNLOADING).toBe('The speech model is still downloading.');
  expect(NO_DOWNLOAD).toBe('The speech model is already installed or downloading.');
  expect(MODEL_BYTES).toBe(1_624_555_275);
  expect(partFailed(2, 3, 'boom')).toBe('Part 2 of 3 could not be transcribed: boom. Retry to try again.');
  expect(leftover(1)).toBe(
    'The last session was not transcribed. Retry to transcribe its 1 recorded part, or start a new session.',
  );
  expect(leftover(2)).toContain('its 2 recorded parts,');
  expect(COPIED).toBe('Copied to the clipboard.');
  expect(NOTHING_HEARD).toBe(
    'No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone.',
  );
});

test('segments join as paragraphs in order, silent ones left out', () => {
  expect(join(['a', null, 'b'])).toBe('a\n\nb');
  expect(join([null])).toBe('');
  expect(heard(0, false)).toBe('This is part 1 of the dictation.');
  expect(heard(1, true).length).toBeGreaterThan(1000);
});

test('a finished session copies its transcript, and a background result is announced', async () => {
  const mock = new Mock('');
  const events: Snapshot[] = [];
  mock.onChange((s) => events.push(s));
  await mock.act('new', '');
  await mock.act('record', '');
  const paused = await mock.act('pause', '');
  expect(paused.pending, 'in flight').toBe(1);
  await settled();
  expect(events.map((e) => [e.phase, e.pending, e.revision])).toEqual([['paused', 0, paused.revision + 1]]);
  await mock.act('resume', '');
  await mock.act('done', '');
  await settled();
  expect(mock.snapshot().text).toBe(`${heard(0, false)}\n\n${heard(1, false)}`);
  expect(mock.handle.copied).toEqual([mock.snapshot().text]);
});

test('a late result from a discarded session changes nothing', async () => {
  const mock = new Mock('');
  mock.handle.hold('transcribe');
  await mock.act('new', '');
  await mock.act('record', '');
  await mock.act('pause', '');
  await mock.act('discard', '');
  await mock.act('record', '');
  await mock.act('pause', '');
  mock.handle.release('transcribe');
  await settled();
  const s = mock.snapshot();
  expect([s.segments, s.pending]).toEqual([1, 0]);
});

test('the mock identifies itself, and named calls fail on request', async () => {
  expect(await new Mock('').version()).toBe(MOCK_VERSION);
  expect(MOCK_VERSION).toMatch(/mock/);
  await expect(new Mock('?fail=app_version').version()).rejects.toThrow('Mock failure: app_version');
  await expect(new Mock('?fail=dictation_state').state()).rejects.toThrow('Mock failure: dictation_state');
  await expect(new Mock('?fail=new').act('new', '')).rejects.toThrow('Mock failure: new');
  await expect(new Mock('').state()).resolves.toMatchObject({ phase: 'idle' });
});

test('a held call waits for release', async () => {
  const mock = new Mock('');
  mock.handle.hold('dictation_act');
  let done = false;
  const pending = mock.act('new', '').then(() => {
    done = true;
  });
  await settled();
  expect(done).toBe(false);
  mock.handle.release('dictation_act');
  await pending;
  expect(done).toBe(true);
});

test('a missing model downloads at launch with progress, and Record waits for it', async () => {
  const mock = new Mock('?model=download');
  const events: Snapshot[] = [];
  mock.onChange((e) => events.push(e));
  expect(mock.snapshot()).toMatchObject({ revision: 0, modelError: null, download: { receivedBytes: 0, totalBytes: MODEL_BYTES } });
  await mock.act('new', '');
  await expect(mock.act('record', '')).rejects.toThrow(DOWNLOADING);
  await expect(mock.act('downloadModel', '')).rejects.toThrow(NO_DOWNLOAD);
  await finished(mock);
  const received = events.map((e) => e.download?.receivedBytes ?? null);
  expect(received).toEqual([
    ...Array.from({ length: DOWNLOAD_STEPS }, (_, i) => Math.floor((MODEL_BYTES * (i + 1)) / DOWNLOAD_STEPS)),
    null,
  ]);
  expect(events.map((e) => e.revision)).toEqual([2, 3, 4, 5, 6]);
  expect(mock.snapshot()).toMatchObject({ modelError: null, download: null, notice: null });
  await expect(mock.act('record', '')).resolves.toMatchObject({ phase: 'recording' });
  await expect(mock.act('downloadModel', '')).rejects.toThrow(NO_DOWNLOAD);
});

test('a failed download is an error with Retry download, which then succeeds', async () => {
  const mock = new Mock('?model=fail');
  await finished(mock);
  const failed = mock.snapshot();
  expect(failed.modelError).toBe(downloadFailed(DOWNLOAD_REASON));
  await mock.act('new', '');
  await expect(mock.act('record', '')).rejects.toThrow(downloadFailed(DOWNLOAD_REASON));
  const retrying = await mock.act('downloadModel', '');
  expect(retrying).toMatchObject({ modelError: null, download: { receivedBytes: 0 } });
  await finished(mock);
  expect(mock.snapshot().modelError).toBeNull();
});

test('while recording the mock microphone sends a level every LEVEL_MS, and stops when the recording does', async () => {
  const mock = new Mock('');
  const levels: number[] = [];
  mock.onLevel((l) => levels.push(l));
  await mock.act('new', '');
  await mock.act('record', '');
  await new Promise((resolve) => setTimeout(resolve, LEVEL_MS * 5));
  await mock.act('pause', '');
  const heardWhileRecording = levels.length;
  expect(heardWhileRecording).toBeGreaterThan(0);
  expect(levels).toEqual(levels.map((_, n) => syntheticLevel(n)));
  await new Promise((resolve) => setTimeout(resolve, LEVEL_MS * 3));
  expect(levels.length, 'silent while paused').toBe(heardWhileRecording);
  await mock.act('resume', '');
  await mock.act('discard', '');
  await mock.act('record', '');
  await mock.act('done', '');
  await new Promise((resolve) => setTimeout(resolve, LEVEL_MS * 3));
  expect(levels.length, 'silent once discarded or done').toBe(heardWhileRecording);
});

test('the mock logs every action asked for, allowed or not', async () => {
  const mock = new Mock('');
  await mock.act('new', '');
  await expect(mock.act('pause', '')).rejects.toThrow();
  expect(mock.handle.acts).toEqual(['new', 'pause']);
});

// ---- the recording library -----------------------------------------------------

test('excerpts follow the core rule (library.rs)', () => {
  const a = (n: number): string => 'a'.repeat(n);
  const b = (n: number): string => 'b'.repeat(n);
  const cases: [string, string][] = [
    ['  Hello\n\n world  ', 'Hello world'],
    ['abcdefghij abcdefghij abcdefgh', 'abcdefghij abcdefghij abcdefgh'],
    ['This is part 1 of the dictation.', 'This is part 1 of the...'],
    ["Okay, here's what I need. First, check the board; then, once it's done, let me know.", "Okay, here's what I need..."],
    [`${a(19)} ${b(20)}`, `${a(19)} ${b(10)}...`],
    [`${a(20)} ${b(20)}`, `${a(20)}...`],
    ['Supercalifragilisticexpialidocious is long', 'Supercalifragilisticexpialidoc...'],
    ['Abcdefghijklmnopqrstuvwxyzabc, more', 'Abcdefghijklmnopqrstuvwxyzabc...'],
    ['é'.repeat(31), `${'é'.repeat(30)}...`],
    [`${a(29)}😀bbb`, `${a(29)}😀...`],
  ];
  for (const [text, want] of cases) expect(excerpt(text), JSON.stringify(text)).toBe(want);
  expect(excerpt('a\u0085b'), 'U+0085 is whitespace').toBe('a b');
  expect(excerpt('a\uFEFFb'), 'U+FEFF is not').toBe('a\uFEFFb');
});

test('titles are local time, zero-padded', () => {
  expect(title(new Date(2026, 0, 5, 9, 3, 7))).toBe('2026-01-05 09:03:07');
  expect(title(new Date(2026, 11, 31, 23, 59, 59))).toBe('2026-12-31 23:59:59');
  expect(preloaded(1)).toEqual({
    id: '20261026-085900',
    title: '2026-10-26 08:59:00',
    summary: 'Preloaded recording number 2.',
    transcript: 'This is preloaded recording number 2.',
  });
});

/** A whole session in the mock: New, Record, Done, transcribed. */
async function session(mock: Mock): Promise<void> {
  await mock.act('new', '');
  await mock.act('record', '');
  await mock.act('done', '');
  await settled();
}

test('a finished session is archived with its excerpt, then summarized', async () => {
  const mock = new Mock('');
  mock.handle.hold('summarize');
  const events: Snapshot[] = [];
  mock.onChange((s) => events.push(s));
  await session(mock);
  expect(mock.snapshot().libraryRevision).toBe(1);
  const [entry] = await mock.libraryList();
  expect(entry?.summary).toBe('This is part 1 of the...');
  expect(await mock.libraryGet(entry?.id ?? '')).toBe(heard(0, false));
  mock.handle.release('summarize');
  await settled();
  expect((await mock.libraryList())[0]?.summary).toBe(MOCK_SUMMARY);
  expect(events.at(-1)?.libraryRevision).toBe(2);
});

test('colliding ids take a suffix and list newest first', async () => {
  const mock = new Mock('?summaries=off');
  for (let i = 0; i < 3; i++) await session(mock);
  const ids = (await mock.libraryList()).map((e) => e.id);
  expect(ids).toHaveLength(3);
  expect(ids[1]).toBe(`${ids[2] ?? ''}-2`);
  expect(ids[0]).toBe(`${ids[2] ?? ''}-3`);
  expect(mock.snapshot().libraryRevision).toBe(3);
});

test('summaries off, or Ollama down, keep the excerpt; nothing is archived from an empty session', async () => {
  for (const params of ['?summaries=off', '?ollama=down']) {
    const mock = new Mock(params);
    await session(mock);
    await settled();
    expect((await mock.libraryList())[0]?.summary, params).toBe('This is part 1 of the...');
  }
  const silent = new Mock('?mic=silent');
  await session(silent);
  expect(await silent.libraryList()).toEqual([]);
});

test('settings round-trip, and the library calls fail on request', async () => {
  const mock = new Mock('?libraryOpen=0&summaries=off');
  expect(await mock.settingsGet()).toEqual({ summaries: false, libraryOpen: false, vocabulary: MOCK_VOCABULARY, cleanup: false });
  expect(await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: true })).toEqual({
    summaries: true,
    libraryOpen: true,
    vocabulary: '/x/terms.txt',
    cleanup: true,
  });
  expect(await mock.settingsGet()).toEqual({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: true });
  await expect(mock.libraryGet('../segments')).rejects.toThrow(NOT_IN_LIBRARY);
  for (const call of ['library_list', 'library_get', 'library_copy', 'settings_get', 'settings_set'] as const) {
    const failing = new Mock(`?fail=${call}&library=1`);
    const attempt = {
      library_list: () => failing.libraryList(),
      library_get: () => failing.libraryGet('20261026-090000'),
      library_copy: () => failing.libraryCopy('20261026-090000'),
      settings_get: () => failing.settingsGet(),
      settings_set: () => failing.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '', cleanup: false }),
    }[call];
    await expect(attempt(), call).rejects.toThrow(`Mock failure: ${call}`);
  }
  const copying = new Mock('?library=1');
  await copying.libraryCopy('20261026-090000');
  expect(copying.handle.copied).toEqual(['This is preloaded recording number 1.']);
  await expect(new Mock('?library=1&clipboard=fail').libraryCopy('20261026-090000')).rejects.toThrow(
    'Could not copy to the clipboard: pasteboard unavailable',
  );
});

test('the vocabulary file is kept like the engine keeps it', async () => {
  const mock = new Mock('');
  expect((await mock.settingsGet()).vocabulary, 'the default, as a path').toBe(MOCK_VOCABULARY);
  await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: false });
  expect((await mock.settingsSet({ summaries: false, libraryOpen: true, vocabulary: '', cleanup: false })).vocabulary, "'' keeps it").toBe('/x/terms.txt');
  expect(await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: MOCK_VOCABULARY, cleanup: false })).toEqual({
    summaries: true,
    libraryOpen: true,
    vocabulary: MOCK_VOCABULARY,
    cleanup: false,
  });
  // Stored as '': a later '' keeps the default rather than an old path.
  expect((await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '', cleanup: false })).vocabulary).toBe(MOCK_VOCABULARY);
  expect(mock.handle.saved.map((s) => s.vocabulary), 'saved as sent').toEqual(['/x/terms.txt', '', MOCK_VOCABULARY, '']);
});

test('cleanup starts off, or on with ?cleanup=on', async () => {
  expect((await new Mock('').settingsGet()).cleanup).toBe(false);
  expect((await new Mock('?cleanup=on').settingsGet()).cleanup).toBe(true);
});

test('with cleanup on, a held cleanup keeps the session transcribing until released', async () => {
  const mock = new Mock('?cleanup=on');
  mock.handle.hold('cleanup');
  const events: Snapshot[] = [];
  mock.onChange((s) => events.push(s));
  await session(mock);
  const held = mock.snapshot();
  expect([held.phase, held.pending, held.text]).toEqual(['transcribing', 0, '']);
  expect(mock.handle.copied).toEqual([]);
  mock.handle.release('cleanup');
  await settled();
  const done = mock.snapshot();
  expect([done.phase, done.text]).toEqual(['finished', 'This is part 1 of the dictation.']);
  expect(mock.handle.copied).toEqual(['This is part 1 of the dictation.']);
  expect(events.at(-1)?.revision).toBe(done.revision);
  expect(done.revision).toBeGreaterThan(held.revision);
});

test('the open panel picks ?pick=, a default, or nothing, and fails on request', async () => {
  const mock = new Mock('');
  expect(await mock.pickVocabularyFile(MOCK_VOCABULARY)).toBe('/mock/Documents/terms.txt');
  expect(mock.handle.browsed, 'where the panel started').toEqual([MOCK_VOCABULARY]);
  expect(await new Mock('?pick=/a/b.txt').pickVocabularyFile(MOCK_VOCABULARY)).toBe('/a/b.txt');
  expect(await new Mock('?pick=cancel').pickVocabularyFile(MOCK_VOCABULARY)).toBeNull();
  await expect(new Mock('?fail=pick_vocabulary_file').pickVocabularyFile(MOCK_VOCABULARY)).rejects.toThrow(
    'Mock failure: pick_vocabulary_file',
  );
});

test('opening the vocabulary file records the resolved path, or fails like the engine', async () => {
  const mock = new Mock('');
  await mock.vocabularyOpen();
  await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: false });
  await mock.vocabularyOpen();
  expect(mock.handle.opened).toEqual([MOCK_VOCABULARY, '/x/terms.txt']);
  const failing = new Mock('?fail=vocabulary_open');
  await expect(failing.vocabularyOpen()).rejects.toThrow('Could not open the vocabulary file: no text editor.');
  expect(failing.handle.opened).toEqual([]);
});
