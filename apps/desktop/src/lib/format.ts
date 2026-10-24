// Display rules for the dictation screen. Pure functions, unit-tested in
// tests/unit/format.spec.ts.

import type { Action, Phase, Snapshot } from './types';

/**
 * The actions the core accepts in each phase (crates/scriber-core/src/dictation.rs).
 * The window disables every other button; the core refuses them anyway.
 */
export const LEGAL: Readonly<Record<Phase, readonly Action[]>> = {
  idle: ['new'],
  ready: ['record', 'discard'],
  recording: ['pause', 'done', 'discard'],
  paused: ['resume', 'done', 'discard'],
  transcribing: [],
  finished: ['new', 'discard', 'copy'],
  failed: ['new', 'discard', 'retry'],
};

export function allowed(phase: Phase, action: Action): boolean {
  return LEGAL[phase].includes(action);
}

/** Recorded time as m:ss (h:mm:ss from an hour), rounded down to the second. */
export function formatElapsed(ms: number): string {
  const total = Math.floor(Math.max(0, ms) / 1000);
  const s = String(total % 60).padStart(2, '0');
  const m = Math.floor(total / 60) % 60;
  const h = Math.floor(total / 3600);
  return h > 0 ? `${String(h)}:${String(m).padStart(2, '0')}:${s}` : `${String(m)}:${s}`;
}

/** "1 part", "2 parts". */
export function parts(n: number): string {
  return `${String(n)} ${n === 1 ? 'part' : 'parts'}`;
}

/** The status line for a snapshot. */
export function statusText(s: Snapshot): string {
  switch (s.phase) {
    case 'idle':
      return 'Click New to start a dictation.';
    case 'ready':
      return 'Click Record and start talking.';
    case 'recording':
      return 'Recording. Pause to think, Done when you have said it all.';
    case 'paused':
      return s.pending > 0
        ? `Paused. Transcribing ${parts(s.pending)} while you think.`
        : `Paused after ${parts(s.segments)}. Resume when you are ready.`;
    case 'transcribing':
      return `Transcribing the last ${parts(s.pending)}…`;
    case 'finished':
      return s.notice ?? 'The transcript is ready.';
    case 'failed':
      return 'The transcript is not ready.';
  }
}
