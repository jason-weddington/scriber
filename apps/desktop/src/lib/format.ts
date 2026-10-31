// Display rules for the dictation screen. Pure functions, unit-tested in
// tests/unit/format.spec.ts.

import type { Action, Download, InputDevices, LibraryEntry, Phase, Snapshot } from './types';

/**
 * The actions the core accepts in each phase (crates/scriber-core/src/dictation.rs).
 * The window offers only the buttons controlsFor gives for the phase, except
 * New, which stays in the band, disabled, and Copy and Undo, which are offered
 * outside the recorder row (the transcript box and the learned line); the core
 * refuses anything else anyway.
 */
export const LEGAL: Readonly<Record<Phase, readonly Action[]>> = {
  idle: ['new'],
  ready: ['record', 'discard'],
  recording: ['pause', 'done', 'discard'],
  paused: ['resume', 'done', 'discard'],
  transcribing: [],
  finished: ['new', 'discard', 'copy', 'undoLearn'],
  failed: ['new', 'discard', 'retry'],
};

export type Control = { action: Action; label: string; rank: 'primary' | 'secondary' | 'quiet' };

const DISCARD: Control = { action: 'discard', label: 'Discard', rank: 'quiet' };

/** The buttons the recorder row offers in each phase, in DOM order. */
export const CONTROLS: Readonly<Record<Phase, readonly Control[]>> = {
  idle: [],
  ready: [{ action: 'record', label: 'Record', rank: 'primary' }, DISCARD],
  recording: [
    { action: 'pause', label: 'Pause', rank: 'primary' },
    { action: 'done', label: 'Done', rank: 'secondary' },
    DISCARD,
  ],
  paused: [
    { action: 'resume', label: 'Resume', rank: 'primary' },
    { action: 'done', label: 'Done', rank: 'secondary' },
    DISCARD,
  ],
  transcribing: [],
  finished: [DISCARD],
  failed: [{ action: 'retry', label: 'Retry', rank: 'primary' }, DISCARD],
};

export function controlsFor(phase: Phase): readonly Control[] {
  return CONTROLS[phase];
}

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
      return s.notice ?? 'Recording. Pause to think, Done when you have said it all.';
    case 'paused':
      return s.pending > 0
        ? `Paused. Transcribing ${parts(s.pending)} while you think.`
        : `Paused after ${parts(s.segments)}. Resume when you are ready.`;
    case 'transcribing':
      return s.pending > 0 ? `Transcribing the last ${parts(s.pending)}…` : 'Transcribing…';
    case 'finished':
      return s.notice ?? 'The transcript is ready.';
    case 'failed':
      return 'The transcript is not ready.';
  }
}

// ---- the microphone ------------------------------------------------------------------

/**
 * The microphone the control names: while recording, the one the core opened;
 * otherwise (paused included) the one the next Record or Resume will open: the
 * saved choice when the list has it, else the Mac default.
 */
export function activeMicrophone(snapshot: Snapshot, inputs: InputDevices | null, saved: string): string | null {
  if (snapshot.phase === 'recording' && snapshot.microphone !== null) return snapshot.microphone;
  if (inputs?.others.includes(saved)) return saved;
  return inputs?.defaultDevice ?? null;
}

/** The menu's first item: `Mac default (Jabra Evolve 65)`, or `Mac default` when there is none. */
export function defaultMicrophoneLabel(name: string | null): string {
  return name === null ? 'Mac default' : `Mac default (${name})`;
}

// ---- the input level waveform ------------------------------------------------

/** A bar's width in px. */
export const BAR_PX = 4;

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

/** The Settings shortcut, as `aria-keyshortcuts` names it and as the gear's tooltip shows it. */
export const SETTINGS_SHORTCUT = { aria: 'Meta+Comma', hint: '⌘,' };

/** Whether a key press is ⌘, (and nothing else held): it opens Settings in every phase. */
export function opensSettings(press: KeyPress): boolean {
  return press.metaKey && !press.ctrlKey && !press.altKey && !press.shiftKey && press.key === ',';
}

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

// ---- the library drawer ------------------------------------------------------------

/** The drawer's default width in px (crates/scriber-core/src/settings.rs). */
export const LIBRARY_WIDTH_DEFAULT = 220;

/** The narrowest the drawer may be, in px. */
export const LIBRARY_WIDTH_MIN = 110;

/** The widest the drawer may be, in px. */
export const LIBRARY_WIDTH_MAX = 440;

/** How far an arrow key moves the drawer's edge, in px. */
export const LIBRARY_WIDTH_STEP = 10;

/** `px` rounded to a whole number of px and held to the allowed range; the bridge's u64 refuses a fraction. */
export function clampLibraryWidth(px: number): number {
  return Math.min(LIBRARY_WIDTH_MAX, Math.max(LIBRARY_WIDTH_MIN, Math.round(px)));
}

const TITLE = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/;

/**
 * The day group a library entry belongs to, from its title (local time,
 * `YYYY-MM-DD HH:MM:SS`) and `now`'s local date: Today, Yesterday, `Oct 26`
 * (with the year when it is not now's), or Earlier for a title that is not
 * well formed. Field ranges are not checked: the parts go to `new Date`.
 */
export function dayLabel(title: string, now: Date): string {
  if (!TITLE.test(title)) return 'Earlier';
  const [y = 0, m = 0, d = 0] = title.slice(0, 10).split('-').map(Number);
  const day = new Date(y, m - 1, d);
  const same = (a: Date, b: Date): boolean =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (same(day, now)) return 'Today';
  if (same(day, new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1))) return 'Yesterday';
  return day.toLocaleDateString('en-US', {
    month: 'short',
    day: 'numeric',
    ...(day.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
  });
}

/** The title's time as `9:44 AM` (an ASCII space; Intl would put U+202F there), or a malformed title unchanged. */
export function timeLabel(title: string): string {
  if (!TITLE.test(title)) return title;
  const [hh, mm] = title.slice(11, 16).split(':');
  const h = Number(hh);
  return `${String(h % 12 || 12)}:${mm ?? ''} ${h < 12 ? 'AM' : 'PM'}`;
}

/** A row's text: its summary, or `Untitled recording` when that is blank. */
export function rowSummary(summary: string): string {
  return summary.trim() === '' ? 'Untitled recording' : summary;
}

/** `entries` grouped by day label, in list order, the groups in order of first appearance. */
export function groupByDay(entries: LibraryEntry[], now: Date): { label: string; entries: LibraryEntry[] }[] {
  const groups: { label: string; entries: LibraryEntry[] }[] = [];
  for (const entry of entries) {
    const label = dayLabel(entry.title, now);
    const group = groups.find((g) => g.label === label);
    if (group === undefined) groups.push({ label, entries: [entry] });
    else group.entries.push(entry);
  }
  return groups;
}

/** The last component of `path` (split on `/` only), or the whole path when it has none. */
export function fileName(path: string): string {
  const name = path.slice(path.lastIndexOf('/') + 1);
  return name === '' ? path : name;
}
