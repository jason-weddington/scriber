import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

import { DEFAULT_TOGGLE_SHORTCUT, LEGAL } from '../../src/lib/format';
import {
  COPIED,
  DEFAULT_LEARNED_VOCABULARY,
  DEFAULT_MODEL_FOLDER,
  DOWNLOAD_REASON,
  DOWNLOAD_STEP_MS,
  DOWNLOAD_STEPS,
  DOWNLOADING,
  downloadFailed,
  enforceTerms,
  excerpt,
  heard,
  inputFallback,
  join,
  learnCandidates,
  learnTerms,
  LEVEL_MS,
  leftover,
  MAX_LEARNED,
  MAX_RUN_WORDS,
  MAX_RUNS,
  MIN_FIX_CHARS,
  Mock,
  MOCK_DEFAULT_INPUT,
  MOCK_DEFAULT_MODEL,
  MOCK_INPUTS,
  MOCK_MODELS,
  MOCK_SUMMARY,
  MOCK_VOCABULARY,
  MOCK_VERSION,
  MODEL_BYTES,
  modelMissing,
  NO_DOWNLOAD,
  NO_INPUT,
  NOT_IN_LIBRARY,
  NOTHING_HEARD,
  partFailed,
  storedShortcut,
  storedSplit,
  storedWidth,
  preloaded,
  refusal,
  rememberTerms,
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
const ACTIONS: Action[] = ['new', 'record', 'pause', 'resume', 'done', 'discard', 'retry', 'copy', 'accept', 'undoLearn'];

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
  finished: ['new', 'discard', 'copy', 'accept', 'undoLearn'],
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
  accept: 'accept',
  downloadModel: 'download the speech model',
  undoLearn: 'undo learning',
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
    const allowedTypes = types([shapes.snapshot ?? {}, shapes.snapshotFailed ?? {}, shapes.snapshotPaused ?? {}, shapes.snapshotDownloading ?? {}]);
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

  test('the input devices have the fixture keys and types', async () => {
    const allowedTypes = types([shapes.inputDevices ?? {}, shapes.inputDevicesNone ?? {}]);
    for (const params of ['', '?inputs=many', '?inputs=none']) {
      const got = await new Mock(params).inputDevices();
      expect(Object.keys(got).sort(), `${params} keys`).toEqual([...allowedTypes.keys()].sort());
      for (const [k, v] of Object.entries(got)) {
        expect(allowedTypes.get(k)?.has(v === null ? 'null' : typeof v), `${params} ${k}`).toBe(true);
      }
      expect(Array.isArray(got.others), `${params} others`).toBe(true);
      for (const name of got.others) expect(typeof name).toBe('string');
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
  expect(await mock.settingsGet()).toEqual({ summaries: false, libraryOpen: false, vocabulary: MOCK_VOCABULARY, cleanup: false, summaryModel: MOCK_DEFAULT_MODEL, cleanupModel: MOCK_DEFAULT_MODEL, libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  expect(await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: true, summaryModel: 'gemma3:4b', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).toEqual({
    summaries: true,
    libraryOpen: true,
    vocabulary: '/x/terms.txt',
    cleanup: true,
    summaryModel: 'gemma3:4b',
    cleanupModel: MOCK_DEFAULT_MODEL,
    libraryWidth: 220,
    inputDevice: '',
    silenceSplitMs: 0,
    keepSegments: false,
    learnedVocabulary: DEFAULT_LEARNED_VOCABULARY,
    modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT
  });
  expect(await mock.settingsGet()).toEqual({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: true, summaryModel: 'gemma3:4b', cleanupModel: MOCK_DEFAULT_MODEL, libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  await expect(mock.libraryGet('../segments')).rejects.toThrow(NOT_IN_LIBRARY);
  for (const call of ['library_list', 'library_get', 'library_copy', 'settings_get', 'settings_set'] as const) {
    const failing = new Mock(`?fail=${call}&library=1`);
    const attempt = {
      library_list: () => failing.libraryList(),
      library_get: () => failing.libraryGet('20261026-090000'),
      library_copy: () => failing.libraryCopy('20261026-090000'),
      settings_get: () => failing.settingsGet(),
      settings_set: () => failing.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT }),
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
  await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  expect((await mock.settingsSet({ summaries: false, libraryOpen: true, vocabulary: '', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).vocabulary, "'' keeps it").toBe('/x/terms.txt');
  expect(await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: MOCK_VOCABULARY, cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).toEqual({
    summaries: true,
    libraryOpen: true,
    vocabulary: MOCK_VOCABULARY,
    cleanup: false,
    summaryModel: MOCK_DEFAULT_MODEL,
    cleanupModel: MOCK_DEFAULT_MODEL,
    libraryWidth: 220,
    inputDevice: '',
    silenceSplitMs: 0,
    keepSegments: false,
    learnedVocabulary: DEFAULT_LEARNED_VOCABULARY,
    modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT
  });
  // Stored as '': a later '' keeps the default rather than an old path.
  expect((await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).vocabulary).toBe(MOCK_VOCABULARY);
  expect(mock.handle.saved.map((s) => s.vocabulary), 'saved as sent').toEqual(['/x/terms.txt', '', MOCK_VOCABULARY, '']);
});

test('the learned vocabulary file is kept like the engine keeps it', async () => {
  const mock = new Mock('');
  expect((await mock.settingsGet()).learnedVocabulary, 'the default, as a path').toBe(DEFAULT_LEARNED_VOCABULARY);
  expect((await new Mock('?learnedVocabulary=/x/learned.txt').settingsGet()).learnedVocabulary).toBe('/x/learned.txt');
  await mock.settingsSet({ ...(await mock.settingsGet()), vocabulary: '/x/terms.txt', learnedVocabulary: '/x/terms.txt' });
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), learnedVocabulary: '' })).learnedVocabulary, "'' keeps it").toBe('/x/terms.txt');
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).learnedVocabulary).toBe(DEFAULT_LEARNED_VOCABULARY);
  // Stored as '': a later '' keeps the default rather than an old path.
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), learnedVocabulary: '' })).learnedVocabulary).toBe(DEFAULT_LEARNED_VOCABULARY);
  expect(mock.handle.saved.map((s) => s.learnedVocabulary), 'saved as sent').toEqual(['/x/terms.txt', '', DEFAULT_LEARNED_VOCABULARY, '']);
});

test('opening the learned vocabulary file records the resolved path, or fails like the engine', async () => {
  const mock = new Mock('');
  await mock.learnedVocabularyOpen();
  await mock.settingsSet({ ...(await mock.settingsGet()), learnedVocabulary: '/x/learned.txt' });
  await mock.learnedVocabularyOpen();
  expect(mock.handle.opened, 'the resolved learned path').toEqual([DEFAULT_LEARNED_VOCABULARY, '/x/learned.txt']);
  const failing = new Mock('?fail=learned_vocabulary_open');
  await expect(failing.learnedVocabularyOpen()).rejects.toThrow('Could not open the vocabulary file: no text editor.');
  expect(failing.handle.opened).toEqual([]);
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

test('the model folder starts at ?modelFolder= or the default, and an empty one resolves to the default', async () => {
  expect((await new Mock('').settingsGet()).modelFolder).toBe(DEFAULT_MODEL_FOLDER);
  expect((await new Mock('?modelFolder=/x/models').settingsGet()).modelFolder).toBe('/x/models');
  expect((await new Mock('?modelFolder=').settingsGet()).modelFolder, "'' answers the default").toBe(DEFAULT_MODEL_FOLDER);
});

test('the model folder is kept like the engine keeps it', async () => {
  const mock = new Mock('');
  await mock.settingsSet({ ...(await mock.settingsGet()), modelFolder: '/x/models', toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), modelFolder: '', toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).modelFolder, "'' keeps it").toBe('/x/models');
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).modelFolder).toBe(DEFAULT_MODEL_FOLDER);
  // Stored as '': a later '' keeps the default rather than an old path.
  expect((await mock.settingsSet({ ...(await mock.settingsGet()), modelFolder: '', toggleShortcut: DEFAULT_TOGGLE_SHORTCUT })).modelFolder).toBe(DEFAULT_MODEL_FOLDER);
  expect(mock.handle.saved.map((s) => s.modelFolder), 'saved as sent').toEqual(['/x/models', '', DEFAULT_MODEL_FOLDER, '']);
});

type ModelInternals = {
  modelPresent: boolean;
  received: number | null;
  modelError(): string | null;
};

test('the missing-model sentence names the resolved model folder, and follows a change', async () => {
  const mock = new Mock('?model=download');
  const internals = mock as unknown as ModelInternals;
  internals.modelPresent = false;
  internals.received = null;
  expect(internals.modelError()).toBe(modelMissing(`${DEFAULT_MODEL_FOLDER}/ggml-large-v3-turbo.bin`));
  await mock.settingsSet({ ...(await mock.settingsGet()), modelFolder: '/x/models', toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  expect(internals.modelError()).toBe(modelMissing('/x/models/ggml-large-v3-turbo.bin'));
});

test('the model folder panel picks ?picked=, a default, or nothing, and fails on request', async () => {
  const mock = new Mock('');
  expect(await mock.pickModelFolder('/data/models')).toBe('/mock/Models');
  expect(mock.handle.browsed, 'where the panel started').toEqual(['/data/models']);
  const picked = new Mock('?picked=/a/models');
  expect(await picked.pickModelFolder('/data/models')).toBe('/a/models');
  expect(picked.handle.browsed).toEqual(['/data/models']);
  const cancelled = new Mock('?picked=cancel');
  expect(await cancelled.pickModelFolder('/data/models')).toBeNull();
  expect(cancelled.handle.browsed).toEqual(['/data/models']);
  const failing = new Mock('?fail=pick_model_folder');
  await expect(failing.pickModelFolder('/data/models')).rejects.toThrow('Mock failure: pick_model_folder');
  expect(failing.handle.browsed).toEqual(['/data/models']);
});

test('opening the vocabulary file records the resolved path, or fails like the engine', async () => {
  const mock = new Mock('');
  await mock.vocabularyOpen();
  await mock.settingsSet({ summaries: true, libraryOpen: true, vocabulary: '/x/terms.txt', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT });
  await mock.vocabularyOpen();
  expect(mock.handle.opened).toEqual([MOCK_VOCABULARY, '/x/terms.txt']);
  const failing = new Mock('?fail=vocabulary_open');
  await expect(failing.vocabularyOpen()).rejects.toThrow('Could not open the vocabulary file: no text editor.');
  expect(failing.handle.opened).toEqual([]);
});

test('the models are kept like the engine keeps them', async () => {
  const mock = new Mock('');
  const got = await mock.settingsGet();
  expect([got.summaryModel, got.cleanupModel], 'the defaults, by name').toEqual([MOCK_DEFAULT_MODEL, MOCK_DEFAULT_MODEL]);
  expect((await new Mock('?summaryModel=llama3:70b').settingsGet()).summaryModel).toBe('llama3:70b');
  expect((await new Mock('?cleanupModel=llama3:70b').settingsGet()).cleanupModel).toBe('llama3:70b');
  const base = { summaries: true, libraryOpen: true, vocabulary: '', cleanup: false, libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: DEFAULT_LEARNED_VOCABULARY, modelFolder: DEFAULT_MODEL_FOLDER, toggleShortcut: DEFAULT_TOGGLE_SHORTCUT };
  await mock.settingsSet({ ...base, summaryModel: 'gemma3:4b', cleanupModel: 'llama3:8b' });
  const kept = await mock.settingsSet({ ...base, summaryModel: '', cleanupModel: '' });
  expect([kept.summaryModel, kept.cleanupModel], "'' keeps them").toEqual(['gemma3:4b', 'llama3:8b']);
  await mock.settingsSet({ ...base, summaryModel: MOCK_DEFAULT_MODEL, cleanupModel: MOCK_DEFAULT_MODEL });
  // Stored as '': a later '' keeps the default rather than an old model.
  const after = await mock.settingsSet({ ...base, summaryModel: '', cleanupModel: '', libraryWidth: 220, inputDevice: '', silenceSplitMs: 0, keepSegments: false });
  expect([after.summaryModel, after.cleanupModel]).toEqual([MOCK_DEFAULT_MODEL, MOCK_DEFAULT_MODEL]);
  const read = await mock.settingsGet();
  expect([read.summaryModel, read.cleanupModel]).toEqual([MOCK_DEFAULT_MODEL, MOCK_DEFAULT_MODEL]);
  expect(
    mock.handle.saved.map((s) => s.summaryModel),
    'saved as sent',
  ).toEqual(['gemma3:4b', '', MOCK_DEFAULT_MODEL, '']);
});

test('the installed models are listed, counted, and fail under ?ollama=down', async () => {
  const mock = new Mock('');
  const handle = mock.handle;
  expect(handle.modelListings).toBe(0);
  expect(await mock.ollamaModels()).toEqual(MOCK_MODELS);
  expect(handle.modelListings).toBe(1);
  expect(await mock.ollamaModels()).toEqual(MOCK_MODELS);
  expect(handle.modelListings).toBe(2);
  const down = new Mock('?ollama=down');
  await expect(down.ollamaModels()).rejects.toThrow(/^Local AI needs Ollama: connection refused\.$/);
  expect(down.handle.modelListings).toBe(1);
});

test('a held listing waits for its release', async () => {
  const mock = new Mock('?hold=ollama_models');
  let got: string[] | null = null;
  const listing = mock.ollamaModels().then((m) => {
    got = m;
  });
  await settled();
  expect(got).toBeNull();
  mock.handle.release('ollama_models');
  await listing;
  expect(got).toEqual(MOCK_MODELS);
});

test('full screen: starts as the URL says, fails and holds like any call, and tells its listeners', async () => {
  await expect(new Mock('?fullscreen=1').fullscreen()).resolves.toBe(true);
  await expect(new Mock('').fullscreen()).resolves.toBe(false);
  await expect(new Mock('?fail=window_fullscreen').fullscreen()).rejects.toThrow(/^Mock failure: window_fullscreen$/);

  const mock = new Mock('');
  const heard: boolean[] = [];
  const stop = mock.onFullscreen((on) => heard.push(on));
  mock.handle.setFullscreen(true);
  expect(heard).toEqual([true]);
  await expect(mock.fullscreen()).resolves.toBe(true);
  stop();
  mock.handle.setFullscreen(false);
  expect(heard).toEqual([true]);

  const held = new Mock('?hold=window_fullscreen');
  let got: boolean | null = null;
  const read = held.fullscreen().then((on) => {
    got = on;
  });
  await settled();
  expect(got).toBeNull();
  held.handle.release('window_fullscreen');
  await read;
  expect(got).toBe(false);
});

test('the drawer width is read like the core reads it', async () => {
  for (const [n, want] of [[300, 300], [109, 110], [110, 110], [440, 440], [441, 440], [-1, 220], [1.5, 220]] as const) {
    expect(storedWidth(n), String(n)).toBe(want);
  }
  expect((await new Mock('').settingsGet()).libraryWidth).toBe(220);
  expect((await new Mock('?libraryWidth=300').settingsGet()).libraryWidth).toBe(300);
  const mock = new Mock('');
  const current = await mock.settingsGet();
  expect((await mock.settingsSet({ ...current, libraryWidth: 5000 })).libraryWidth, 'clamped on read').toBe(440);
  expect(mock.handle.saved.map((s) => s.libraryWidth), 'saved as sent').toEqual([5000]);
});

// ---- the microphone ---------------------------------------------------------------

test('the input devices answer the constants, or the ?inputs= scenario, and count their calls', async () => {
  const mock = new Mock('');
  expect(await mock.inputDevices()).toEqual({ defaultDevice: MOCK_DEFAULT_INPUT, others: [...MOCK_INPUTS] });
  expect(MOCK_DEFAULT_INPUT).toBe('Jabra Evolve 65');
  expect(MOCK_INPUTS).toEqual(['AirPods Pro', 'MacBook Pro Microphone']);
  expect(mock.handle.inputListings).toBe(1);
  const many = await new Mock('?inputs=many').inputDevices();
  expect(many.defaultDevice).toBe(MOCK_DEFAULT_INPUT);
  expect(many.others).toHaveLength(30);
  expect([many.others[0], many.others[29]]).toEqual(['Microphone 01', 'Microphone 30']);
  expect(await new Mock('?inputs=long').inputDevices()).toEqual({ defaultDevice: MOCK_DEFAULT_INPUT, others: ['L'.repeat(80)] });
  expect(await new Mock('?inputs=none').inputDevices()).toEqual({ defaultDevice: null, others: [] });
  mock.handle.setInputs(['Other']);
  expect((await mock.inputDevices()).others).toEqual(['Other']);
  expect(mock.handle.inputListings).toBe(2);
  const failing = new Mock('?fail=input_devices');
  await expect(failing.inputDevices()).rejects.toThrow('Mock failure: input_devices');
  expect(failing.handle.inputListings).toBe(1);
});

test('a held input_devices call waits for release', async () => {
  const mock = new Mock('');
  mock.handle.hold('input_devices');
  let answered = false;
  const pending = mock.inputDevices().then(() => {
    answered = true;
  });
  await settled();
  expect(answered).toBe(false);
  mock.handle.release('input_devices');
  await pending;
  expect(answered).toBe(true);
});

test('the input device is saved verbatim, seeded by ?inputDevice=', async () => {
  expect((await new Mock('').settingsGet()).inputDevice).toBe('');
  expect((await new Mock('?inputDevice=AirPods%20Pro').settingsGet()).inputDevice).toBe('AirPods Pro');
  const mock = new Mock('?inputDevice=AirPods%20Pro');
  const sent = { ...(await mock.settingsGet()), inputDevice: '' };
  expect((await mock.settingsSet(sent)).inputDevice, "'' is a value").toBe('');
  expect((await mock.settingsGet()).inputDevice).toBe('');
});

/** A mock with `params`, recording. */
async function recording(params: string): Promise<Mock> {
  const mock = new Mock(params);
  await mock.act('new', '');
  await mock.act('record', '');
  return mock;
}

test('record opens the saved input device like the core', async () => {
  const fresh = new Mock('');
  expect(fresh.snapshot().microphone, 'none before the first record').toBeNull();
  const byDefault = await recording('');
  expect([byDefault.snapshot().microphone, byDefault.snapshot().notice]).toEqual([MOCK_DEFAULT_INPUT, null]);
  const chosen = await recording('?inputDevice=AirPods%20Pro');
  expect([chosen.snapshot().microphone, chosen.snapshot().notice]).toEqual(['AirPods Pro', null]);
  const theDefault = await recording(`?inputDevice=${encodeURIComponent(MOCK_DEFAULT_INPUT)}`);
  expect([theDefault.snapshot().microphone, theDefault.snapshot().notice]).toEqual([MOCK_DEFAULT_INPUT, null]);
  const gone = await recording('?inputDevice=Gone%20Mic');
  expect([gone.snapshot().microphone, gone.snapshot().notice]).toEqual([MOCK_DEFAULT_INPUT, inputFallback('Gone Mic', MOCK_DEFAULT_INPUT)]);
  expect(inputFallback('Jabra Evolve 65', 'MacBook Pro Microphone')).toBe(
    'Jabra Evolve 65 is not available, so this part is recording from the Mac default, MacBook Pro Microphone.',
  );
  expect((await gone.settingsGet()).inputDevice, 'the choice is kept').toBe('Gone Mic');
});

test('with no input device, record and resume are refused and change nothing', async () => {
  const mock = new Mock('?inputs=none');
  await mock.act('new', '');
  const before = mock.snapshot();
  await expect(mock.act('record', '')).rejects.toThrow(NO_INPUT);
  expect(NO_INPUT).toBe('Could not start recording: there is no input device');
  expect(mock.snapshot()).toEqual(before);
  const gone = new Mock('?inputs=none&inputDevice=Gone%20Mic');
  await gone.act('new', '');
  await expect(gone.act('record', '')).rejects.toThrow(NO_INPUT);
  expect(gone.snapshot().notice, 'no fallback notice').toBeNull();
});

test('the microphone follows resume, and new and discard clear it', async () => {
  const mock = await recording('');
  await mock.act('pause', '');
  expect(mock.snapshot().microphone, 'kept while paused').toBe(MOCK_DEFAULT_INPUT);
  await mock.settingsSet({ ...(await mock.settingsGet()), inputDevice: 'AirPods Pro' });
  await mock.act('resume', '');
  expect(mock.snapshot().microphone).toBe('AirPods Pro');
  await mock.act('discard', '');
  expect(mock.snapshot().microphone, 'discard').toBeNull();
  await mock.act('record', '');
  await mock.act('done', '');
  await settled();
  expect(mock.snapshot().microphone, 'kept when finished').toBe('AirPods Pro');
  await mock.act('new', '');
  expect(mock.snapshot().microphone, 'new').toBeNull();
});

test('the fallback notice goes at pause and returns at a resume that falls back', async () => {
  const mock = await recording('?inputDevice=Gone%20Mic');
  await mock.act('pause', '');
  expect(mock.snapshot().notice).toBeNull();
  await mock.act('resume', '');
  expect(mock.snapshot().notice).toBe(inputFallback('Gone Mic', MOCK_DEFAULT_INPUT));
  mock.handle.setInputs(['Gone Mic']);
  await mock.act('pause', '');
  await mock.act('resume', '');
  expect([mock.snapshot().microphone, mock.snapshot().notice], 'listed again').toEqual(['Gone Mic', null]);
});

// ---- learning (crates/scriber-core/tests/learn.rs, the same tables) ----------

const SIX = 'Use TightBeam and Scribe and Github and Svelt and Ollama and Cargo now.';

const LEARN_CORPUS: [string, string, string[]][] = [
  ['Open TightBeam now.', 'Open tightbeam now.', ['tightbeam']],
  ['The tight beam runner.', 'The tightbeam runner.', ['tightbeam']],
  ['Ask the scribe app.', 'Ask the Scriber app.', ['Scriber']],
  ['The build was fast.', 'The build was quick.', []],
  ['The cat sat.', 'the cat sat.', []],
  ["TightBeam's queue.", "tightbeam's queue.", ['tightbeam']],
  [SIX, 'Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.', []],
  [
    SIX,
    'Use tightbeam and Scriber and GitHub and Svelte and ollama and Cargo now.',
    ['tightbeam', 'Scriber', 'GitHub', 'Svelte', 'ollama'],
  ],
  ['Open the app.', 'Open the new app.', []],
  ['Open the old app.', 'Open the app.', []],
  ['Run tightbeam now.', 'Run tight beam now.', ['tight beam']],
  ['The tight-beam runner.', 'The tightbeam runner.', ['tightbeam']],
  ['Check the g t d board.', 'Check the GTD board.', ['GTD']],
  ['Say alpha beta gamma delta now.', 'Say one two three four now.', []],
  ['Send it to Tom.', 'Send it to Tim.', []],
  ['A bird flew.', 'A word flew.', []],
  ['Ping Ollamma now.', 'Ping Ollama now.', ['Ollama']],
  ['Done? Yes.', 'Done? yes.', []],
  ['Hello there.', 'hello there.', []],
  ['Done. Hello.', 'Done. hello.', []],
  ['Open TightBeam and TightBeam.', 'Open tightbeam and Tightbeam.', ['tightbeam']],
  ['Ask Zoë now.', 'Ask Zoe now.', []],
  ['Open Münchenn now.', 'Open München now.', ['München']],
  ['Same text.', 'Same text.', []],
  ['Ask scribe about it. "Done." Github is up and it is fast', 'Ask Scriber about it. "Done." github is up and it is quick', ['Scriber']],
  ['one two three', 'one two three four five', []],
  ['x tightbeam y z', 'x y tight beam z', []],
  ['a b c', 'c b a', []],
  ["it's fine", 'Its fine', []],
  ['New Scribe!', 'New Scriber!', ['Scriber']],
  ['\u0130stanbul now.', 'i\u0307stanbul now.', ['i\u0307stanbul']],
];

const REMEMBER: [string[], string[], string[], string[], string[]][] = [
  [[], ['tightbeam'], [], ['tightbeam'], ['tightbeam']],
  [['Scriber', 'TightBeam'], ['tightbeam'], [], ['tightbeam', 'Scriber'], ['tightbeam']],
  [['Scriber', 'tightbeam'], ['tightbeam'], [], ['tightbeam', 'Scriber'], ['tightbeam']],
  [[], ['tightbeam'], ['tightbeam'], [], []],
  [[], ['tightbeam'], ['TightBeam'], ['tightbeam'], ['tightbeam']],
];

const ENFORCE: [string[], string, string][] = [
  [['tightbeam'], 'Open TightBeam, then tight beam and Tight-Beam.', 'Open tightbeam, then tightbeam and tightbeam.'],
  [['tightbeam'], "TightBeam's queue", "tightbeam's queue"],
  [['tightbeam'], 'tightbeams and xtightbeam', 'tightbeams and xtightbeam'],
  [['tightbeam'], 'foo-tightbeam', 'foo-tightbeam'],
  [['tightbeam'], 'Tightbeam works.', 'tightbeam works.'],
  [['tight beam'], 'tightbeam', 'tight beam'],
  [['Scriber'], 'the scriber app', 'the Scriber app'],
  [['v2.0'], 'V2.0', 'V2.0'],
  [[], 'Open TightBeam.', 'Open TightBeam.'],
  [['München'], 'in münchen today', 'in München today'],
  [['tightbeam'], 'tightbeam_x and x_tightbeam', 'tightbeam_x and x_tightbeam'],
  [['tightbeam'], 'tight  beam', 'tight  beam'],
  [['tightbeam'], 'tight beam-', 'tight beam-'],
  [['GTD'], 'the g t d board', 'the GTD board'],
  [['ab'], 'a b c d', 'ab c d'],
  [['abcd'], 'a b c d', 'a b c d'],
  [['', '-'], 'a - b', 'a - b'],
  [['tight beam', 'TightBeam'], 'TIGHTBEAM', 'tight beam'],
  [['Scrib', 'Scriber'], 'scriber scrib', 'Scriber Scrib'],
  [['ab', 'abc'], 'a b c', 'abc'],
];

test('learning follows the core rule (learn.rs)', () => {
  expect([MAX_RUN_WORDS, MAX_RUNS, MIN_FIX_CHARS, MAX_LEARNED]).toEqual([3, 5, 4, 100]);
  for (const [original, edited, expected] of LEARN_CORPUS) {
    expect(learnTerms(original, edited), `${original} => ${edited}`).toEqual(expected);
  }
  expect(learnCandidates(SIX, 'Use tightbeam and Scriber and GitHub and Svelte and ollama and cargo now.')).toEqual([
    'tightbeam',
    'Scriber',
    'GitHub',
    'Svelte',
    'ollama',
    'cargo',
  ]);
  expect(learnCandidates('Open TightBeam and TightBeam.', 'Open tightbeam and Tightbeam.')).toEqual(['tightbeam', 'Tightbeam']);
  for (const [list, learned, vocabulary, next, added] of REMEMBER) {
    expect(rememberTerms(list, learned, vocabulary), `${String(list)} + ${String(learned)}`).toEqual([next, added]);
  }
  for (const size of [100, 99]) {
    const list = Array.from({ length: size }, (_, k) => `t${String(k + 1)}`);
    const [next, added] = rememberTerms(list, ['new'], []);
    expect(next.length, `${String(size)}: capped`).toBe(100);
    expect([next[0], next.at(-1)], `${String(size)}: ends`).toEqual(['new', 't99']);
    expect(added).toEqual(['new']);
  }
  for (const [terms, text, expected] of ENFORCE) {
    expect(enforceTerms(text, terms), `${String(terms)}: ${text}`).toEqual(expected);
  }
});

test('a correction copied is learned, enforced next session, and undone on request', async () => {
  const mock = new Mock('');
  for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
  await settled();
  const shown = mock.snapshot().text;
  expect(shown).toBe(heard(0, false));
  const fixed = shown.replace('dictation', 'Dictation');
  const copied = await mock.act('copy', fixed);
  expect(copied.learned, 'learned').toEqual(['Dictation']);
  expect(copied.notice).toBe(COPIED);
  const again = await mock.act('copy', fixed);
  expect(again.learned, 'a second Copy learns nothing new and keeps it').toEqual(['Dictation']);
  const undone = await mock.act('undoLearn', '');
  expect(undone.learned, 'undone').toEqual([]);
  expect(undone.notice, 'notice unchanged').toBe(COPIED);
  const nothing = await mock.act('undoLearn', '');
  expect(nothing.revision, 'nothing to undo').toBe(undone.revision + 1);
  await mock.act('copy', fixed.replace('This', 'this'));
  expect(mock.snapshot().learned, 'a sentence start is not learned').toEqual([]);
  for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
  await settled();
  expect(mock.snapshot().text, 'nothing enforced after Undo').toBe(heard(0, false));
  await mock.act('copy', fixed);
  expect(mock.snapshot().learned).toEqual(['Dictation']);
  const fresh = await mock.act('new', '');
  expect(fresh.learned, 'New clears it').toEqual([]);
  for (const step of ['record', 'done'] as const) await mock.act(step, '');
  await settled();
  expect(mock.snapshot().text, 'enforced').toBe(fixed);
  await mock.act('copy', `${fixed} one two three four five six`);
  await mock.act('copy', SIX);
  await mock.act('copy', SIX.replace('TightBeam and Scribe and Github and Svelt and Ollama and Cargo', 'tightbeam and Scriber and GitHub and Svelte and ollama and cargo'));
  expect(mock.snapshot().learned, 'a rewrite is capped').toEqual([]);
  await mock.act('discard', '');
  expect(mock.snapshot().learned).toEqual([]);
});


test('partial follows the heard segments in order, and text and rawText stay empty until finish', async () => {
  const mock = new Mock('');
  for (const step of ['new', 'record', 'pause'] as const) await mock.act(step, '');
  await settled();
  const first = mock.snapshot();
  expect(first.partial).toBe(heard(0, false));
  expect(first.text).toBe('');
  expect(first.rawText).toBeNull();
  for (const step of ['resume', 'pause'] as const) await mock.act(step, '');
  await settled();
  expect(mock.snapshot().partial).toBe(`${heard(0, false)}\n\n${heard(1, false)}`);
  const fresh = await mock.act('discard', '');
  expect(fresh.partial).toBe('');
  expect(fresh.rawText).toBeNull();
});

test('a failed segment contributes nothing to partial', async () => {
  const mock = new Mock('?transcribe=fail');
  for (const step of ['new', 'record', 'pause'] as const) await mock.act(step, '');
  await settled();
  expect(mock.snapshot().partial).toBe('');
});

test('rawText is the text before learned spellings, non-null whenever finished', async () => {
  const mock = new Mock('');
  for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
  await settled();
  const done = mock.snapshot();
  expect(done.text).toBe(heard(0, false));
  expect(done.rawText).toBe(heard(0, false));
  expect((await mock.act('copy', 'Edited.')).rawText).toBe(heard(0, false));
  expect((await mock.act('copy', heard(0, false))).rawText).toBe(heard(0, false));
});

test('rawText keeps what was heard when a learned spelling is enforced', async () => {
  const mock = new Mock('');
  for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
  await settled();
  const learned = await mock.act('copy', 'This is part 1 of the Dictation.');
  expect(learned.rawText).toBe(heard(0, false));
  for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
  await settled();
  const second = mock.snapshot();
  expect(second.text).toBe('This is part 1 of the Dictation.');
  expect(second.rawText).toBe(heard(0, false));
});

test('accept learns like copy but writes no clipboard and sets no notice', async () => {
  for (const params of ['', '?clipboard=fail']) {
    const mock = new Mock(params);
    for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
    await settled();
    const before = mock.snapshot();
    const copiedBefore = [...mock.handle.copied];
    const fixed = before.text.replace('dictation', 'Dictation');
    const accepted = await mock.act('accept', fixed);
    expect(accepted.learned).toEqual(['Dictation']);
    expect(accepted.text).toBe(fixed);
    expect(accepted.notice).toBe(before.notice);
    expect(mock.handle.copied).toEqual(copiedBefore);
    const undone = await mock.act('undoLearn', '');
    expect(undone.learned).toEqual([]);
    for (const step of ['new', 'record', 'done'] as const) await mock.act(step, '');
    await settled();
    expect(mock.snapshot().text, 'not enforced after the undo').toBe(heard(0, false));
  }
});

test('accept is refused from idle', async () => {
  const mock = new Mock('');
  await expect(mock.act('accept', 'x')).rejects.toThrow('Cannot accept while idle.');
});

test('a split closes the running part at a silence while recording goes on, and its text fills in partial', async () => {
  const mock = new Mock('?silenceSplitMs=1500');
  for (const step of ['new', 'record'] as const) await mock.act(step, '');
  await new Promise((resolve) => setTimeout(resolve, LEVEL_MS * 3 + 20));
  const before = mock.snapshot();
  mock.handle.split();
  const split = mock.snapshot();
  expect(split.phase).toBe('recording');
  expect(split.segments).toBe(1);
  expect(split.recordedMs).toBeGreaterThanOrEqual(LEVEL_MS * 3);
  expect(split.revision).toBe(before.revision + 1);
  await settled();
  expect(mock.snapshot().phase).toBe('recording');
  expect(mock.snapshot().partial).toBe(heard(0, false));
  const paused = await mock.act('pause', '');
  expect(paused.recordedMs).toBeGreaterThanOrEqual(split.recordedMs);
  expect(paused.segments).toBe(2);
});

test('with the split off, or while paused, split() changes nothing', async () => {
  const off = new Mock('');
  for (const step of ['new', 'record'] as const) await off.act(step, '');
  const before = off.snapshot();
  off.handle.split();
  expect(off.snapshot()).toEqual(before);
  await off.act('pause', '');

  const paused = new Mock('?silenceSplitMs=1500');
  for (const step of ['new', 'record', 'pause'] as const) await paused.act(step, '');
  await settled();
  const still = paused.snapshot();
  paused.handle.split();
  expect(paused.snapshot()).toEqual(still);
});

test('the silence split is stored like the core reads it', async () => {
  const mock = new Mock('');
  const base = await mock.settingsGet();
  expect(base.silenceSplitMs).toBe(0);
  expect((await mock.settingsSet({ ...base, silenceSplitMs: 3000 })).silenceSplitMs).toBe(3000);
  for (const ms of [500, 10001, 1500.5]) {
    expect((await mock.settingsSet({ ...base, silenceSplitMs: ms })).silenceSplitMs, String(ms)).toBe(0);
  }
  expect((await new Mock('?silenceSplitMs=2500').settingsGet()).silenceSplitMs).toBe(2500);
});

test('keepSegments is hidden: seeded from the URL, never changed by settingsSet', async () => {
  expect((await new Mock('?keepSegments=1').settingsGet()).keepSegments).toBe(true);
  expect((await new Mock('').settingsGet()).keepSegments).toBe(false);
  const on = new Mock('?keepSegments=1');
  const sentOff = { ...(await on.settingsGet()), keepSegments: false };
  expect((await on.settingsSet(sentOff)).keepSegments).toBe(true);
  expect((await on.settingsGet()).keepSegments).toBe(true);
  const off = new Mock('');
  expect((await off.settingsSet({ ...(await off.settingsGet()), keepSegments: true })).keepSegments).toBe(false);
  expect((await off.settingsGet()).keepSegments).toBe(false);
});

test('after a split, Pause closes the next part and Done joins both', async () => {
  const mock = new Mock('?silenceSplitMs=1500');
  for (const step of ['new', 'record'] as const) await mock.act(step, '');
  mock.handle.split();
  const paused = await mock.act('pause', '');
  expect(paused.segments).toBe(2);
  await mock.act('done', '');
  await settled();
  const done = mock.snapshot();
  expect(done.phase).toBe('finished');
  expect(done.text).toBe(`${heard(0, false)}\n\n${heard(1, false)}`);
});

test('the split is read at Record and Resume, not while recording', async () => {
  const mock = new Mock('');
  for (const step of ['new', 'record'] as const) await mock.act(step, '');
  await mock.settingsSet({ ...(await mock.settingsGet()), silenceSplitMs: 1500 });
  const before = mock.snapshot();
  mock.handle.split();
  expect(mock.snapshot()).toEqual(before);
  for (const step of ['pause', 'resume'] as const) await mock.act(step, '');
  mock.handle.split();
  expect(mock.snapshot().segments).toBe(2);
  expect(mock.snapshot().phase).toBe('recording');
  await mock.act('discard', '');
  await mock.act('record', '');
  await mock.act('discard', '');
  const ready = mock.snapshot();
  mock.handle.split();
  expect(mock.snapshot(), 'nothing recording after Discard').toEqual(ready);
});

test('storedSplit keeps 0 or a whole number of ms in range', () => {
  for (const [n, want] of [
    [0, 0],
    [1000, 1000],
    [1500, 1500],
    [10000, 10000],
    [999, 0],
    [10001, 0],
    [-1500, 0],
    [1500.5, 0],
    [Number.NaN, 0],
  ] as const) {
    expect(storedSplit(n), String(n)).toBe(want);
  }
});

test('the default toggle shortcut is the fixture default, and starts changed only by request', async () => {
  expect(DEFAULT_TOGGLE_SHORTCUT).toBe(shapes.settings?.toggleShortcut);
  expect((await new Mock('').settingsGet()).toggleShortcut).toBe('Opt+Cmd+D');
  expect((await new Mock('?toggleShortcut=Opt+Cmd+K').settingsGet()).toggleShortcut).toBe('Opt+Cmd+K');
  expect((await new Mock('?toggleShortcut=nonsense').settingsGet()).toggleShortcut).toBe('Opt+Cmd+D');
});

test('the toggle shortcut is kept like the engine keeps it', async () => {
  for (const value of ['Opt+Cmd+D', 'Opt+Cmd+K', 'Ctrl+Opt+Shift+Cmd+F13', 'Opt+Cmd+Space']) {
    expect(storedShortcut(value), value).toBe(value);
  }
  for (const value of ['', 'nonsense', 'D', 'Cmd+Opt', 'Cmd+Cmd+D', 'Meta+D', 'Opt+Cmd+X+', 'Opt+Cmd+1']) {
    expect(storedShortcut(value), value).toBe(DEFAULT_TOGGLE_SHORTCUT);
  }
  const mock = new Mock('');
  await mock.settingsSet({ ...(await mock.settingsGet()), toggleShortcut: 'Opt+Cmd+K' });
  expect((await mock.settingsGet()).toggleShortcut, 'a saved value').toBe('Opt+Cmd+K');
  expect(mock.handle.saved.at(-1)?.toggleShortcut, 'saved verbatim').toBe('Opt+Cmd+K');
  await mock.settingsSet({ ...(await mock.settingsGet()), toggleShortcut: 'nonsense' });
  expect((await mock.settingsGet()).toggleShortcut, 'a stored value the core refuses').toBe(DEFAULT_TOGGLE_SHORTCUT);
});
