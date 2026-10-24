import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { expect, test } from '@playwright/test';

import { LEGAL } from '../../src/lib/format';
import { COPIED, heard, join, leftover, Mock, MOCK_VERSION, modelMissing, NOTHING_HEARD, partFailed, refusal } from '../../src/lib/mock';
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
    const allowedTypes = types([shapes.snapshot ?? {}, shapes.snapshotFailed ?? {}]);
    const snapshots = [new Mock('?model=missing').snapshot(), new Mock('?leftover=1').snapshot()];
    for (const phase of PHASES) snapshots.push((await at(phase)).snapshot());
    for (const s of snapshots) {
      expect(Object.keys(s).sort(), `${s.phase} keys`).toEqual([...allowedTypes.keys()].sort());
      for (const [k, v] of Object.entries(s)) {
        expect(allowedTypes.get(k)?.has(v === null ? 'null' : typeof v), `${s.phase} ${k}`).toBe(true);
      }
    }
  });
});

test('the legal actions are the core table, in the mock and in the buttons', async () => {
  expect(LEGAL).toEqual(CORE_LEGAL);
  for (const phase of PHASES) {
    for (const action of ACTIONS) {
      const mock = await at(phase);
      const before = mock.snapshot();
      const attempt = mock.act(action, 'edited');
      if (CORE_LEGAL[phase].includes(action)) {
        await expect(attempt, `${action} from ${phase}`).resolves.toMatchObject({ revision: before.revision + 1 });
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
  expect(modelMissing('/m.bin')).toBe('Model missing: run scripts/fetch-model.sh to download it to /m.bin.');
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
