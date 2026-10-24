// Display rules for the dictation screen. Pure functions, unit-tested in
// tests/unit/format.spec.ts.

import type { Action, Download, Phase, Snapshot } from './types';

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

// ---- the input level waveform ------------------------------------------------

/** A bar's width in px. */
export const BAR_PX = 2;

/** The gap between bars in px. */
export const GAP_PX = 2;

/** The levels kept: 25.6 s at the core's 20 a second, more than the widest strip draws. */
export const MAX_BARS = 512;

/** How many bars fit a strip `width` px wide, at BAR_PX wide and GAP_PX apart. */
export function barCount(width: number): number {
  if (!Number.isFinite(width) || width <= 0) return 0;
  return Math.min(MAX_BARS, Math.floor((width + GAP_PX) / (BAR_PX + GAP_PX)));
}

/** `levels` with `level` added as the newest (last), keeping the newest MAX_BARS. */
export function pushLevel(levels: readonly number[], level: number): number[] {
  return [...levels, Math.min(1, Math.max(0, level))].slice(-MAX_BARS);
}

/** The `count` bars to draw, oldest first: the newest levels right-aligned, the empty slots at 0. */
export function bars(levels: readonly number[], count: number): number[] {
  if (count <= 0) return [];
  const recent = levels.slice(-count);
  return [...Array<number>(count - recent.length).fill(0), ...recent];
}

// ---- keyboard shortcuts --------------------------------------------------------

/** The parts of a KeyboardEvent a shortcut depends on. */
export type KeyPress = Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'>;

/** Each action's shortcut, as `aria-keyshortcuts` names it and as the button shows it. */
export const SHORTCUTS: Readonly<Partial<Record<Action, { aria: string; hint: string }>>> = {
  new: { aria: 'Meta+N', hint: '⌘N' },
  record: { aria: 'Space', hint: 'Space' },
  pause: { aria: 'Space', hint: 'Space' },
  resume: { aria: 'Space', hint: 'Space' },
  done: { aria: 'Enter', hint: '⏎' },
};

/**
 * The action a key press asks for in `phase`, or null when it asks for none
 * or for one the phase does not allow. ⌘N is New everywhere; Space (Record,
 * Pause, Resume) and Return (Done) only outside text fields (`inText`), where
 * they type.
 */
export function shortcut(press: KeyPress, phase: Phase, inText: boolean): Action | null {
  const modifiers = [press.metaKey, press.ctrlKey, press.altKey, press.shiftKey];
  let action: Action | null = null;
  if (press.metaKey && modifiers.filter(Boolean).length === 1 && press.key.toLowerCase() === 'n') action = 'new';
  else if (modifiers.some(Boolean) || inText) action = null;
  else if (press.key === ' ') action = phase === 'ready' ? 'record' : phase === 'recording' ? 'pause' : 'resume';
  else if (press.key === 'Enter') action = 'done';
  return action !== null && allowed(phase, action) ? action : null;
}

// ---- the speech model's download --------------------------------------------------

const UNITS = ['B', 'kB', 'MB', 'GB', 'TB'] as const;

/** Bytes like Finder: powers of 1000, one decimal below 10, 999.6 MB carried to "1 GB". */
export function formatBytes(bytes: number): string {
  for (let unit = 0; ; unit++) {
    const value = bytes / 1000 ** unit;
    const shown = unit > 0 && value < 10 ? Math.round(value * 10) / 10 : Math.round(value);
    if (shown < 1000 || unit === UNITS.length - 1) return `${String(shown)} ${UNITS[unit] ?? ''}`;
  }
}

/** Whole percent received, never past 100. */
export function percent(d: Download): number {
  return d.totalBytes > 0 ? Math.min(100, Math.floor((d.receivedBytes * 100) / d.totalBytes)) : 0;
}

/** The download's line: why Record waits, and how far it has got. */
export function downloadText(d: Download): string {
  return `Downloading the speech model, needed once: ${String(percent(d))}% (${formatBytes(d.receivedBytes)} of ${formatBytes(d.totalBytes)}). Record works when it is done.`;
}
