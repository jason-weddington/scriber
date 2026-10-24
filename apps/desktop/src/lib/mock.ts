// A deterministic stand-in for the Rust core, used whenever the frontend runs
// outside Tauri (Playwright, a plain browser). It runs the core's dictation
// state machine (crates/scriber-core/src/dictation.rs) over a fake microphone
// and a fake transcriber, and tests/unit/mock.spec.ts holds it to the core:
// same JSON shapes as the Rust fixture, same legal actions, same sentences.
//
// The page URL picks the scenario:
//   ?model=missing        the speech model is not installed
//   ?mic=silent           every segment is silence
//   ?transcribe=fail      each segment's first transcription fails (Retry succeeds)
//   ?clipboard=fail       the clipboard cannot be written
//   ?leftover=<n>         launch finds n segments of an unfinished session
//   ?long=1               each segment is heard as a long paragraph
//   ?fail=<call>          that call rejects: "dictation_state", "app_version" or an action
// A test can hold a call so the UI is seen mid-call, and release it:
//   window.__mockBridge.hold('dictation_act' | 'transcribe') ... release(...)
// Segment durations come from Date.now(), so Playwright's clock controls them.

import type { Action, Phase, Snapshot } from './types';

/** The version the mock reports, so a mock can never pass for the real app. */
export const MOCK_VERSION = '0.0.0-mock';

/** Where the mock says the model belongs. */
export const MOCK_MODEL_PATH = '/Users/me/Library/Application Support/com.jasonweddington.scriber/models/ggml-large-v3-turbo.bin';

/** The core's sentences (crates/scriber-core/src/dictation.rs). */
export const COPIED = 'Copied to the clipboard.';
export const NOTHING_HEARD =
  'No speech was heard. If you spoke, check that Scriber may use the microphone in System Settings, Privacy & Security, Microphone.';

/** How the core names each action in a refusal (`impl Display for Action`). */
export const ACTION_NAMES: Readonly<Record<Action, string>> = {
  new: 'start a new session',
  record: 'record',
  pause: 'pause',
  resume: 'resume',
  done: 'finish',
  discard: 'discard',
  retry: 'retry',
  copy: 'copy',
};

/** The core's legal actions per phase. */
const LEGAL: Readonly<Record<Phase, readonly Action[]>> = {
  idle: ['new'],
  ready: ['record', 'discard'],
  recording: ['pause', 'done', 'discard'],
  paused: ['resume', 'done', 'discard'],
  transcribing: [],
  finished: ['new', 'discard', 'copy'],
  failed: ['new', 'discard', 'retry'],
};

export function refusal(action: Action, phase: Phase): string {
  return `Cannot ${ACTION_NAMES[action]} while ${phase}.`;
}

export function modelMissing(path: string): string {
  return `Model missing: run scripts/fetch-model.sh to download it to ${path}.`;
}

export function partFailed(part: number, total: number, reason: string): string {
  return `Part ${String(part)} of ${String(total)} could not be transcribed: ${reason}. Retry to try again.`;
}

export function leftover(n: number): string {
  return `The last session was not transcribed. Retry to transcribe its ${String(n)} recorded ${n === 1 ? 'part' : 'parts'}, or start a new session.`;
}

/** What the fake transcriber hears in segment `index`. */
export function heard(index: number, long: boolean): string {
  const short = `This is part ${String(index + 1)} of the dictation.`;
  return long ? Array.from({ length: 40 }, () => short).join(' ') : short;
}

/** The core's joining rule: each segment a paragraph, silent ones left out. */
export function join(texts: (string | null)[]): string {
  return texts.filter((t): t is string => t !== null).join('\n\n');
}

type Slot = { state: 'pending' } | { state: 'heard'; text: string | null } | { state: 'failed'; reason: string };

/** What a test reaches through `window.__mockBridge`. */
export interface MockHandle {
  /** Makes `name` ('dictation_act', 'dictation_state' or 'transcribe') wait until `release(name)`. */
  hold(name: string): void;
  release(name: string): void;
  /** Every text the mock clipboard was given, in order. */
  readonly copied: string[];
}

declare global {
  interface Window {
    __mockBridge?: MockHandle;
  }
}

export class Mock {
  private readonly modelMissing: boolean;
  private readonly silent: boolean;
  private readonly flaky: boolean;
  private readonly clipboardFails: boolean;
  private readonly long: boolean;
  private readonly failing: string | null;
  private readonly listeners = new Set<(s: Snapshot) => void>();
  private readonly held = new Map<string, { promise: Promise<void>; open: () => void }>();
  private readonly copied: string[] = [];
  private readonly attempts = new Map<number, number>();

  private phase: Phase = 'idle';
  private slots: Slot[] = [];
  private recordedMs = 0;
  private since: number | null = null;
  private text = '';
  private error: string | null = null;
  private notice: string | null = null;
  private revision = 0;
  /** Bumped by New and Discard, so a late result from a discarded session is ignored. */
  private session = 0;

  constructor(search: string) {
    const params = new URLSearchParams(search);
    this.modelMissing = params.get('model') === 'missing';
    this.silent = params.get('mic') === 'silent';
    this.flaky = params.get('transcribe') === 'fail';
    this.clipboardFails = params.get('clipboard') === 'fail';
    this.long = params.get('long') === '1';
    this.failing = params.get('fail');
    const left = Number(params.get('leftover') ?? '0');
    if (left > 0) {
      this.slots = Array.from({ length: left }, () => ({ state: 'failed', reason: 'interrupted' }));
      this.phase = 'failed';
      this.error = leftover(left);
    }
  }

  get handle(): MockHandle {
    return {
      hold: (name) => {
        let open = (): void => undefined;
        const promise = new Promise<void>((resolve) => {
          open = resolve;
        });
        this.held.set(name, { promise, open });
      },
      release: (name) => {
        this.held.get(name)?.open();
        this.held.delete(name);
      },
      copied: this.copied,
    };
  }

  async version(): Promise<string> {
    await this.gate('app_version');
    if (this.failing === 'app_version') throw new Error('Mock failure: app_version');
    return MOCK_VERSION;
  }

  snapshot(): Snapshot {
    return {
      revision: this.revision,
      phase: this.phase,
      recordedMs: this.recordedMs,
      segments: this.slots.length,
      pending: this.slots.filter((s) => s.state === 'pending').length,
      text: this.text,
      error: this.error,
      notice: this.notice,
      modelError: this.modelMissing ? modelMissing(MOCK_MODEL_PATH) : null,
    };
  }

  private async gate(name: string): Promise<void> {
    await this.held.get(name)?.promise;
  }

  async state(): Promise<Snapshot> {
    await this.gate('dictation_state');
    if (this.failing === 'dictation_state') throw new Error('Mock failure: dictation_state');
    return this.snapshot();
  }

  onChange(cb: (s: Snapshot) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  /** The core's `apply`: refuse what the phase does not allow, else act and answer. */
  async act(action: Action, text: string): Promise<Snapshot> {
    await this.gate('dictation_act');
    if (this.failing === action) throw new Error(`Mock failure: ${action}`);
    if (!LEGAL[this.phase].includes(action)) throw new Error(refusal(action, this.phase));
    const closed: number[] = [];
    switch (action) {
      case 'new':
      case 'discard':
        this.reset();
        break;
      case 'record':
      case 'resume':
        if (this.modelMissing) throw new Error(modelMissing(MOCK_MODEL_PATH));
        this.since = Date.now();
        this.notice = null;
        this.phase = 'recording';
        break;
      case 'pause':
        this.notice = null;
        closed.push(this.close());
        this.phase = 'paused';
        break;
      case 'done':
        this.notice = null;
        if (this.phase === 'recording') closed.push(this.close());
        this.phase = 'transcribing';
        this.settle();
        break;
      case 'retry':
        this.notice = null;
        this.error = null;
        this.slots = this.slots.map((s, i) => {
          if (s.state !== 'failed') return s;
          closed.push(i);
          return { state: 'pending' };
        });
        this.phase = 'transcribing';
        this.settle();
        break;
      case 'copy':
        if (this.clipboardFails) throw new Error('Could not copy to the clipboard: pasteboard unavailable');
        this.copied.push(text);
        this.text = text;
        this.notice = COPIED;
        break;
    }
    this.revision += 1;
    for (const index of closed) void this.transcribe(index);
    return this.snapshot();
  }

  private reset(): void {
    this.session += 1;
    this.slots = [];
    this.attempts.clear();
    this.recordedMs = 0;
    this.since = null;
    this.text = '';
    this.error = null;
    this.notice = null;
    this.phase = 'ready';
  }

  /** Closes the running segment; returns its index. */
  private close(): number {
    this.recordedMs += Date.now() - (this.since ?? Date.now());
    this.since = null;
    this.slots.push({ state: 'pending' });
    return this.slots.length - 1;
  }

  /** The worker: transcribes segment `index` once 'transcribe' is not held. */
  private async transcribe(index: number): Promise<void> {
    const session = this.session;
    await this.gate('transcribe');
    if (session !== this.session || this.slots[index]?.state !== 'pending') return;
    const attempt = (this.attempts.get(index) ?? 0) + 1;
    this.attempts.set(index, attempt);
    this.slots[index] =
      this.flaky && attempt === 1
        ? { state: 'failed', reason: 'model exploded' }
        : { state: 'heard', text: this.silent ? null : heard(index, this.long) };
    this.revision += 1;
    if (this.phase === 'transcribing') this.settle();
    const snapshot = this.snapshot();
    for (const cb of this.listeners) cb(snapshot);
  }

  /** The core's settle: wait for pending, fail on the first failure, else finish and copy. */
  private settle(): void {
    if (this.slots.some((s) => s.state === 'pending')) return;
    const failed = this.slots.findIndex((s) => s.state === 'failed');
    const slot = this.slots[failed];
    if (slot?.state === 'failed') {
      this.error = partFailed(failed + 1, this.slots.length, slot.reason);
      this.phase = 'failed';
      return;
    }
    this.text = join(this.slots.map((s) => (s.state === 'heard' ? s.text : null)));
    this.phase = 'finished';
    if (this.text === '') this.notice = NOTHING_HEARD;
    else if (this.clipboardFails) this.notice = 'Could not copy to the clipboard: pasteboard unavailable. Use Copy to try again.';
    else {
      this.copied.push(this.text);
      this.notice = COPIED;
    }
  }
}
