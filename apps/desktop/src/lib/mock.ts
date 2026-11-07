// A deterministic stand-in for the Rust core, used whenever the frontend runs
// outside Tauri (Playwright, a plain browser). It runs the core's dictation
// state machine (crates/scriber-core/src/dictation.rs) over a fake microphone
// and a fake transcriber, and tests/unit/mock.spec.ts holds it to the core:
// same JSON shapes as the Rust fixture, same legal actions, same sentences.
//
// The page URL picks the scenario:
//   ?model=download       the speech model is missing: it downloads at launch, in 4 steps
//   ?model=fail           the same, but the first download fails half way (Retry download succeeds)
//   ?mic=silent           every segment is silence
//   ?transcribe=fail      each segment's first transcription fails (Retry succeeds)
//   ?clipboard=fail       the clipboard cannot be written
//   ?leftover=<n>         launch finds n segments of an unfinished session
//   ?long=1               each segment is heard as a long paragraph
//   ?fail=<call>          that call rejects: "dictation_state", "app_version", an action,
//                         "library_list", "library_get", "library_copy", "settings_get", "settings_set",
//                         "window_fullscreen", "pick_vocabulary_file" or "pick_model_folder" (with "Mock failure: <call>"); ?fail=vocabulary_open
//                         and ?fail=learned_vocabulary_open reject with the engine's
//                         "Could not open the vocabulary file: no text editor."
//   ?fullscreen=1         the window starts in full screen
//   ?pick=<path>          the open panel picks that file (default /mock/Documents/terms.txt);
//                         ?pick=cancel cancels it
//   ?hold=<call>          that call is held from launch (see hold below)
//   ?library=<n>          the recording library starts with n entries (see preloaded)
//   ?summaries=off        summaries start off in the settings
//   ?libraryOpen=0        the library drawer starts closed in the settings
//   ?libraryWidth=<n>   the drawer starts at that width in the settings
//   ?ollama=down          the summarizer fails, so each entry keeps its excerpt, and ollama_models
//                         rejects with "Local AI needs Ollama: connection refused."
//   ?summaryModel=<name>  the summary model starts as that name in the settings (else '', the default)
//   ?cleanupModel=<name>  the post-processing model starts as that name in the settings (else '')
//   ?cleanup=on           post-processing with the local model starts on in the settings
//   ?inputs=many          input_devices answers MOCK_DEFAULT_INPUT and 30 others, Microphone 01 to Microphone 30
//   ?inputs=long          input_devices answers MOCK_DEFAULT_INPUT and one other with an 80-character name
//   ?inputs=none          input_devices answers no default and no others (Record then fails)
//   ?inputDevice=<name>   the saved input device starts as that name (else '', the Mac default)
//   ?silenceSplitMs=<n>   the silence split starts as n ms in the settings (else 0, off)
//   ?keepSegments=1       the hidden keep_segments setting starts true (else false)
//   ?learnedVocabulary=<path>  the learned list starts at that absolute path (else DEFAULT_LEARNED_VOCABULARY)
//   ?modelFolder=<path>   the model folder starts at that absolute path (else DEFAULT_MODEL_FOLDER)
//   ?toggleShortcut=<value>   the toggle shortcut starts as that Scriber form (else DEFAULT_TOGGLE_SHORTCUT)
//   ?picked=<path>        the model folder panel picks that folder (default /mock/Models); ?picked=cancel cancels it
// window.__mockBridge.split() is the fake microphone closing the running part at a silence: with the
// split read at the last Record or Resume non-zero and the session recording, it closes the part
// (queued for transcription like a Pause) while recording goes on; otherwise it does nothing.
// `pick_vocabulary_file` and `pick_model_folder` are gated the same way, so `?hold=pick_vocabulary_file` / `?hold=pick_model_folder` work though the list above does not name either picker.
// A test can hold a call so the UI is seen mid-call, and release it:
//   window.__mockBridge.hold('dictation_act' | 'transcribe' | 'download' | 'summarize' | 'cleanup' | 'ollama_models' | 'input_devices' | 'window_fullscreen') ... release(...)
// window.__mockBridge.setFullscreen(on) changes the full-screen state and tells the listeners.
// window.__mockBridge.setInputs(others) replaces the other input devices for later listings and Records.
// input_devices counts its calls in window.__mockBridge.inputListings. Record and Resume open the
// saved input device like the core: '' opens the default; a saved name that is the default or
// listed opens it; any other falls back to the default with the core's input_fallback notice; with
// no default they reject with "Could not start recording: there is no input device". The snapshot's
// microphone names what opened, and New and Discard clear it.
// ('download' holds each step of the model's download; 'summarize' holds
// each new library entry's summary; 'cleanup' holds the cleanup pass over a
// finished transcript, with cleanup on, which keeps the session
// transcribing, and reads as postProcessing true; the mock cleaner answers the transcript unchanged.) A finished session with a transcript is
// archived into the library like the core does it (crates/scriber-core/src/library.rs). While recording the
// mock emits a synthetic input level every LEVEL_MS, like the core's 50 ms
// windows. The settings' vocabulary is kept like the engine keeps it: '' for
// the default (answered as MOCK_VOCABULARY), and settings_set's '' keeps the
// saved path while MOCK_VOCABULARY is saved as ''. The learned list is kept
// the same way, answered as DEFAULT_LEARNED_VOCABULARY, and the model folder
// the same way too, answered as DEFAULT_MODEL_FOLDER. Open in Text Editor appends
// the resolved path to window.__mockBridge.opened, and Browse… the path its
// open panel starts at to window.__mockBridge.browsed. The models are kept
// like the engine keeps them: '' for the default (answered as
// MOCK_DEFAULT_MODEL), settings_set's '' keeps the saved model and
// MOCK_DEFAULT_MODEL is saved as ''. ollama_models answers MOCK_MODELS
// ('ollama_models' holds it) and counts its calls in
// window.__mockBridge.modelListings. Segment durations and the level and download timers run on
// Date.now() and setTimeout, so Playwright's clock controls them.
// Learning (crates/scriber-core/src/learn.rs) is ported exactly below: learnCandidates, learnTerms,
// rememberTerms and enforceTerms. The mock keeps its learned list in memory (it starts empty, and the
// mock vocabulary is empty): Copy and Accept on a finished session learn the corrections into it and sets the
// snapshot's learned and an undo list like the core (no log); finish enforces the learned spellings;
// Accept learns the same way but writes no clipboard and sets no notice; undoLearn restores the list as it was before the last learning Copy or Accept; New and Discard clear learned.

import {
  clampLibraryWidth,
  DEFAULT_TOGGLE_SHORTCUT,
  isShortcut,
  LIBRARY_WIDTH_DEFAULT,
  SILENCE_SPLIT_MAX_MS,
  SILENCE_SPLIT_MIN_MS,
} from './format';
import type { Action, Download, InputDevices, LibraryEntry, Phase, Settings, Snapshot } from './types';

/** The version the mock reports, so a mock can never pass for the real app. */
export const MOCK_VERSION = '0.0.0-mock';

/** The model's size, as the core pins it (crates/scriber-core/src/download.rs). */
export const MODEL_BYTES = 1_624_555_275;

/** How often the mock's level arrives while recording: the core's 50 ms window. */
export const LEVEL_MS = 50;

/** The mock download's steps, and the time each takes. */
export const DOWNLOAD_STEPS = 4;
export const DOWNLOAD_STEP_MS = 60;

/** Why the mock's first download fails under ?model=fail. */
export const DOWNLOAD_REASON = 'the connection was reset';

/** The core's sentences (crates/scriber-core/src/dictation.rs). */
export const COPIED = 'Copied to the clipboard.';
export const DOWNLOADING = 'The speech model is still downloading.';
export const NO_DOWNLOAD = 'The speech model is already installed or downloading.';
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
  accept: 'accept',
  downloadModel: 'download the speech model',
  undoLearn: 'undo learning',
};

/** The core's legal actions per phase. */
const LEGAL: Readonly<Record<Phase, readonly Action[]>> = {
  idle: ['new'],
  ready: ['record', 'discard'],
  recording: ['pause', 'done', 'discard'],
  paused: ['resume', 'done', 'discard'],
  transcribing: [],
  finished: ['new', 'discard', 'copy', 'accept', 'undoLearn'],
  failed: ['new', 'discard', 'retry'],
};

/** The core's NotInLibrary sentence (crates/scriber-engine/src/lib.rs). */
export const NOT_IN_LIBRARY = 'That recording is no longer in the library.';

/** The default glossary's absolute path, as the engine answers it. */
export const MOCK_VOCABULARY = '/mock/data/vocabulary.txt';

/** The default learned list's absolute path, as the engine answers it. */
export const DEFAULT_LEARNED_VOCABULARY = '/mock/data/learned.txt';

/** The default speech-model folder, as the engine answers it. */
export const DEFAULT_MODEL_FOLDER = '/mock/data/models';

/** What the mock open panel picks without ?pick=. */
export const MOCK_PICK = '/mock/Documents/terms.txt';

/** The engine's sentence when the text editor cannot open the glossary. */
export const OPEN_FAILED = 'Could not open the vocabulary file: no text editor.';

/** The engine's default Ollama model (DEFAULT_SUMMARY_MODEL), as answered for an unset choice. */
export const MOCK_DEFAULT_MODEL = 'qwen3.8:27b-mlx';

/** The models the mock Ollama has installed, sorted. */
export const MOCK_MODELS: readonly string[] = ['gemma3:4b', 'qwen3.8:27b-mlx'];

/** Why ollama_models rejects under ?ollama=down. */
export const OLLAMA_DOWN = 'Local AI needs Ollama: connection refused.';

/** The mock Mac's default input device. */
export const MOCK_DEFAULT_INPUT = 'Jabra Evolve 65';

/** The mock Mac's other input devices, sorted. */
export const MOCK_INPUTS: readonly string[] = ['AirPods Pro', 'MacBook Pro Microphone'];

/** The core's notice when the chosen input device will not open (dictation.rs input_fallback). */
export function inputFallback(wanted: string, opened: string): string {
  return `${wanted} is not available, so this part is recording from the Mac default, ${opened}.`;
}

/** The core's refusal when no input device opens (the cpal adapter's sentence). */
export const NO_INPUT = 'Could not start recording: there is no input device';

/** What the mock summarizer answers for every entry. */
export const MOCK_SUMMARY = 'A dictated note.';

/** The core's whitespace (`char::is_whitespace`): JavaScript's \s plus U+0085, without U+FEFF. */
function isSpace(c: string): boolean {
  return c !== '\uFEFF' && /[\s\u0085]/u.test(c);
}

/** Every whitespace run as one space, trimmed. */
function collapse(text: string): string {
  const words: string[] = [];
  let word = '';
  for (const c of Array.from(text)) {
    if (isSpace(c)) {
      if (word !== '') words.push(word);
      word = '';
    } else word += c;
  }
  if (word !== '') words.push(word);
  return words.join(' ');
}

/** The core's excerpt (library.rs): whole up to 30 characters, else cut and followed by "...". */
export function excerpt(text: string): string {
  const chars = Array.from(collapse(text));
  if (chars.length <= 30) return chars.join('');
  const head = chars.slice(0, 30);
  const space = head.lastIndexOf(' ');
  const kept = head.slice(0, space >= 20 ? space : 30);
  while (kept.length > 0 && (isSpace(kept.at(-1) ?? '') || ',;:-.'.includes(kept.at(-1) ?? ''))) kept.pop();
  return `${kept.join('')}...`;
}

/** The core's title (library.rs): `date` in local time as `YYYY-MM-DD HH:MM:SS`. */
export function title(date: Date): string {
  const two = (n: number): string => String(n).padStart(2, '0');
  const day = `${String(date.getFullYear()).padStart(4, '0')}-${two(date.getMonth() + 1)}-${two(date.getDate())}`;
  return `${day} ${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

/** The core's base_id: `2026-10-26 14:03:09` becomes `20261026-140309`. */
export function baseId(t: string): string {
  return t.replace(/[-:]/g, '').replace(' ', '-');
}

/** The core's list order: by base id, then by suffix (none counts as 1), newest first. */
function newestFirst(a: LibraryEntry, b: LibraryEntry): number {
  const key = (id: string): [string, number] => [id.slice(0, 15), Number(id.slice(16) || '1')];
  const [ab, an] = key(a.id);
  const [bb, bn] = key(b.id);
  return ab === bb ? bn - an : ab < bb ? 1 : -1;
}

/** Preloaded entry `k` (0-based, newest first): 2026-10-26 09:00:00 local, minus k minutes. */
export function preloaded(k: number): LibraryEntry & { transcript: string } {
  const t = title(new Date(new Date(2026, 9, 26, 9, 0, 0).getTime() - k * 60_000));
  return {
    id: baseId(t),
    title: t,
    summary: `Preloaded recording number ${String(k + 1)}.`,
    transcript: `This is preloaded recording number ${String(k + 1)}.`,
  };
}

export function refusal(action: Action, phase: Phase): string {
  return `Cannot ${ACTION_NAMES[action]} while ${phase}.`;
}

export function modelMissing(path: string): string {
  return `The speech model is missing from ${path}.`;
}

export function downloadFailed(reason: string): string {
  return `Could not download the speech model: ${reason}.`;
}

/** The mock microphone's level for its `n`th window: speech-like swells, deterministic. */
export function syntheticLevel(n: number): number {
  return Math.min(1, 0.1 + 0.75 * Math.abs(Math.sin(n * 0.45)) * (0.7 + 0.3 * Math.sin(n * 0.13)));
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

/** The width the core's `load` reads from `settings.json`: a non-negative integer clamped, else the default. */
export function storedWidth(n: number): number {
  return Number.isInteger(n) && n >= 0 ? clampLibraryWidth(n) : LIBRARY_WIDTH_DEFAULT;
}

/** The silence split the core's `load` reads from `settings.json`: 0, or a whole number of ms in range, else 0. */
export function storedSplit(n: number): number {
  return n === 0 || (Number.isInteger(n) && n >= SILENCE_SPLIT_MIN_MS && n <= SILENCE_SPLIT_MAX_MS) ? n : 0;
}

/** The core's read rule for the toggle shortcut (crates/scriber-core/src/settings.rs). */
export function storedShortcut(value: string): string {
  return isShortcut(value) ? value : DEFAULT_TOGGLE_SHORTCUT;
}

/** The engine's write rule for a model: '' keeps the saved one, the default is saved as ''. */
function stored(sent: string, saved: string): string {
  if (sent === '') return saved;
  return sent === MOCK_DEFAULT_MODEL ? '' : sent;
}

/** The core's learn::MAX_RUN_WORDS, MAX_RUNS, MIN_FIX_CHARS and MAX_LEARNED. */
export const MAX_RUN_WORDS = 3;
export const MAX_RUNS = 5;
export const MIN_FIX_CHARS = 4;
export const MAX_LEARNED = 100;

/** The core's `char::is_alphanumeric`. */
function isAlnum(c: string): boolean {
  return /[\p{Alphabetic}\p{N}]/u.test(c);
}

/** The core's whitespace for splitting words. */
function isWhite(c: string): boolean {
  return /\p{White_Space}/u.test(c);
}

interface LearnWord {
  text: string;
  start: boolean;
}

/** The core's learn::words: stripped words and whether each starts a sentence. */
function learnWords(text: string): LearnWord[] {
  const raws: string[] = [];
  let raw = '';
  for (const c of Array.from(text)) {
    if (isWhite(c)) {
      raws.push(raw);
      raw = '';
    } else raw += c;
  }
  raws.push(raw);
  const out: LearnWord[] = [];
  let afterEnd = true;
  for (const token of raws) {
    const chars = Array.from(token);
    let from = 0;
    let to = chars.length;
    while (from < to && !isAlnum(chars[from] ?? '')) from++;
    while (to > from && !isAlnum(chars[to - 1] ?? '')) to--;
    let word = chars.slice(from, to);
    const n = word.length;
    if (n > 2 && (word[n - 2] === "'" || word[n - 2] === '\u2019') && (word[n - 1] === 's' || word[n - 1] === 'S')) {
      word = word.slice(0, n - 2);
    }
    if (word.length === 0) continue;
    out.push({ text: word.join(''), start: afterEnd });
    const trimmed = Array.from(token);
    while (trimmed.length > 0 && ['"', "'", '\u201D', '\u2019', ')', ']'].includes(trimmed.at(-1) ?? '')) trimmed.pop();
    afterEnd = ['.', '!', '?'].includes(trimmed.at(-1) ?? '');
  }
  return out;
}

/** The core's learn::runs: the unmatched stretches between matched words. */
function learnRuns(original: LearnWord[], edited: LearnWord[]): [LearnWord[], LearnWord[]][] {
  let prefix = 0;
  while (prefix < original.length && prefix < edited.length && original[prefix]?.text === edited[prefix]?.text) prefix++;
  const rest = Math.min(original.length, edited.length) - prefix;
  let suffix = 0;
  while (suffix < rest && original[original.length - 1 - suffix]?.text === edited[edited.length - 1 - suffix]?.text) suffix++;
  const o = original.slice(prefix, original.length - suffix);
  const e = edited.slice(prefix, edited.length - suffix);
  const table = Array.from({ length: o.length + 1 }, () => new Array<number>(e.length + 1).fill(0));
  const lcs = (i: number, j: number): number => table[i]?.[j] ?? 0;
  for (let i = o.length - 1; i >= 0; i--) {
    const row = table[i] ?? [];
    for (let j = e.length - 1; j >= 0; j--) {
      row[j] = o[i]?.text === e[j]?.text ? lcs(i + 1, j + 1) + 1 : Math.max(lcs(i + 1, j), lcs(i, j + 1));
    }
  }
  const out: [LearnWord[], LearnWord[]][] = [];
  let i = 0;
  let j = 0;
  let fromI = 0;
  let fromJ = 0;
  while (i < o.length && j < e.length) {
    if (o[i]?.text === e[j]?.text) {
      out.push([o.slice(fromI, i), e.slice(fromJ, j)]);
      i++;
      j++;
      fromI = i;
      fromJ = j;
    } else if (lcs(i + 1, j) >= lcs(i, j + 1)) i++;
    else j++;
  }
  out.push([o.slice(fromI), e.slice(fromJ)]);
  return out;
}

/** The Levenshtein distance between two code point sequences. */
function levenshtein(a: string[], b: string[]): number {
  let row = Array.from({ length: b.length + 1 }, (_, k) => k);
  a.forEach((ca, i) => {
    const next = [i + 1];
    b.forEach((cb, j) => {
      const diagonal = row[j] ?? 0;
      next.push(ca === cb ? diagonal : Math.min(diagonal, row[j + 1] ?? 0, next[j] ?? 0) + 1);
    });
    row = next;
  });
  return row.at(-1) ?? 0;
}

/** The core's learn::qualifies. */
function qualifies(o: LearnWord[], e: LearnWord[]): boolean {
  if (o.length === e.length && o.every((w, k) => w.text.toLowerCase() === e[k]?.text.toLowerCase())) {
    return o.some((w, k) => {
      const a = Array.from(w.text);
      const b = Array.from(e[k]?.text ?? '');
      if (a.length !== b.length) return true;
      return a.some((c, n) => c !== b[n] && (n !== 0 || !w.start));
    });
  }
  const key = (ws: LearnWord[]): string =>
    Array.from(
      ws
        .map((w) => w.text)
        .join(' ')
        .toLowerCase(),
    )
      .filter((c) => c !== ' ' && c !== '-')
      .join('');
  if (key(o) === key(e)) return true;
  const [a] = o;
  const [b] = e;
  if (o.length === 1 && e.length === 1 && a !== undefined && b !== undefined) {
    if (Array.from(b.text).length < MIN_FIX_CHARS) return false;
    const la = Array.from(a.text.toLowerCase());
    const lb = Array.from(b.text.toLowerCase());
    return levenshtein(la, lb) <= Math.max(1, Math.floor(Math.max(la.length, lb.length) / 3));
  }
  return false;
}

/** The core's learn::candidates: every qualifying term, in document order. */
export function learnCandidates(original: string, edited: string): string[] {
  const inRange = (n: number): boolean => n >= 1 && n <= MAX_RUN_WORDS;
  return learnRuns(learnWords(original), learnWords(edited))
    .filter(([o, e]) => inRange(o.length) && inRange(e.length) && qualifies(o, e))
    .map(([, e]) => e.map((w) => w.text).join(' '));
}

/** The core's learn::learn: nothing past MAX_RUNS, else the candidates de-duplicated case-insensitively. */
export function learnTerms(original: string, edited: string): string[] {
  const found = learnCandidates(original, edited);
  if (found.length > MAX_RUNS) return [];
  const out: string[] = [];
  for (const term of found) if (!out.some((t) => t.toLowerCase() === term.toLowerCase())) out.push(term);
  return out;
}

/** The core's learn::remember: the new list and the terms placed at its head. */
export function rememberTerms(list: string[], learned: string[], vocabulary: string[]): [string[], string[]] {
  const added = learned.filter((t) => !vocabulary.includes(t));
  const lowered = added.map((t) => t.toLowerCase());
  const next = [...added, ...list.filter((t) => !lowered.includes(t.toLowerCase()))].slice(0, MAX_LEARNED);
  return [next, added];
}

/** Whether `c` joins a word, so it cannot border a match. */
function joins(c: string | undefined): boolean {
  return c !== undefined && (isAlnum(c) || c === '_' || c === '-');
}

/** The core's learn::enforce: learned terms rewritten to their spelling, whole words only. */
export function enforceTerms(text: string, terms: string[]): string {
  const keys: [string, string][] = [];
  for (const term of terms) {
    const key = Array.from(term.toLowerCase())
      .filter((c) => c !== ' ' && c !== '-')
      .join('');
    if (key !== '' && Array.from(key).every(isAlnum)) keys.push([term, key]);
  }
  const chars = Array.from(text);
  let out = '';
  let i = 0;
  while (i < chars.length) {
    let best: [number, string] | null = null;
    if (!joins(chars[i - 1])) {
      let joined = '';
      let at = i;
      for (let k = 0; k < 3 && isAlnum(chars[at] ?? ''); k++) {
        while (isAlnum(chars[at] ?? '')) joined += chars[at++] ?? '';
        const lowered = joined.toLowerCase();
        const found = keys.find(([, key]) => key === lowered);
        if (!joins(chars[at]) && found !== undefined) best = [at, found[0]];
        if (chars[at] !== ' ' && chars[at] !== '-') break;
        at++;
      }
    }
    if (best === null) {
      out += chars[i] ?? '';
      i++;
    } else {
      out += best[1];
      i = best[0];
    }
  }
  return out;
}

type Slot = { state: 'pending' } | { state: 'heard'; text: string | null } | { state: 'failed'; reason: string };

/** What a test reaches through `window.__mockBridge`. */
export interface MockHandle {
  /** Makes `name` ('dictation_act', 'dictation_state', 'transcribe', 'download', 'summarize', 'cleanup', 'ollama_models', 'input_devices' or 'window_fullscreen') wait until `release(name)`. */
  hold(name: string): void;
  release(name: string): void;
  /** Every text the mock clipboard was given, in order. */
  readonly copied: string[];
  /** Every action the window asked for, in order, whether or not it was allowed. */
  readonly acts: Action[];
  /** Every settings_set call's settings, in order, whether or not it failed. */
  readonly saved: Settings[];
  /** Every glossary path opened in the text editor, in order. */
  readonly opened: string[];
  /** Where each open panel started (the path it was given), in order. */
  readonly browsed: string[];
  /** How many times ollama_models was called. */
  readonly modelListings: number;
  /** How many times input_devices was called. */
  readonly inputListings: number;
  /** Replaces the other input devices for later listings and Records. */
  setInputs(others: string[]): void;
  /** Puts the window in or out of full screen and tells the listeners. */
  setFullscreen(on: boolean): void;
  /** The fake microphone closes the running part at a silence. */
  split(): void;
}

declare global {
  interface Window {
    __mockBridge?: MockHandle;
  }
}

export class Mock {
  private readonly downloadFails: boolean;
  private readonly silent: boolean;
  private readonly flaky: boolean;
  private readonly clipboardFails: boolean;
  private readonly long: boolean;
  private readonly failing: string | null;
  private readonly listeners = new Set<(s: Snapshot) => void>();
  private readonly levelListeners = new Set<(level: number) => void>();
  private readonly acts: Action[] = [];
  private readonly held = new Map<string, { promise: Promise<void>; open: () => void }>();
  private readonly copied: string[] = [];
  private readonly saved: Settings[] = [];
  private readonly opened: string[] = [];
  private readonly browsed: string[] = [];
  private readonly pick: string | null;
  private readonly picked: string | null;
  private readonly attempts = new Map<number, number>();
  private readonly ollamaDown: boolean;
  private entries: (LibraryEntry & { transcript: string })[] = [];
  private libraryRevision = 0;
  private modelListings = 0;
  private inputListings = 0;
  private inputs: InputDevices;
  private microphone: string | null = null;
  private settings: Settings;
  /** Date.now() at the session's first Record, else null. */
  private started: number | null = null;
  /** The mock's learned.txt, newest first. */
  private learnedList: string[] = [];
  /** The terms the last learning Copy added, until Undo, New or Discard. */
  private learned: string[] = [];
  /** The learned list before the last learning Copy, for Undo. */
  private undo: string[] | null = null;

  private phase: Phase = 'idle';
  private slots: Slot[] = [];
  private recordedMs = 0;
  private since: number | null = null;
  /** The silence split read at the last Record or Resume, like the core; 0 is off. */
  private splitMs = 0;
  private text = '';
  /** Whisper's text of the finished session, before the learned spellings (the core's raw). */
  private raw: string | null = null;
  /** The core's cleaning: the cleanup job is outstanding. */
  private cleaning = false;
  private error: string | null = null;
  private notice: string | null = null;
  private revision = 0;
  /** Bumped by New and Discard, so a late result from a discarded session is ignored. */
  private session = 0;
  private modelPresent: boolean;
  /** Bytes received while the model downloads, else null. */
  private received: number | null = null;
  private downloadError: string | null = null;
  private downloads = 0;
  private fullscreenOn: boolean;
  private readonly fullscreenListeners = new Set<(on: boolean) => void>();
  private levelTimer: ReturnType<typeof setInterval> | null = null;
  private levelCount = 0;

  constructor(search: string) {
    const params = new URLSearchParams(search);
    // ?hold=<name> holds a call from the start, before launch's download can begin.
    for (const name of params.getAll('hold')) this.handle.hold(name);
    const model = params.get('model');
    this.modelPresent = model !== 'download' && model !== 'fail';
    this.downloadFails = model === 'fail';
    this.silent = params.get('mic') === 'silent';
    this.flaky = params.get('transcribe') === 'fail';
    this.clipboardFails = params.get('clipboard') === 'fail';
    this.long = params.get('long') === '1';
    this.failing = params.get('fail');
    this.fullscreenOn = params.get('fullscreen') === '1';
    this.ollamaDown = params.get('ollama') === 'down';
    this.settings = {
      summaries: params.get('summaries') !== 'off',
      libraryOpen: params.get('libraryOpen') !== '0',
      vocabulary: '',
      cleanup: params.get('cleanup') === 'on',
      summaryModel: params.get('summaryModel') ?? '',
      cleanupModel: params.get('cleanupModel') ?? '',
      libraryWidth: Number(params.get('libraryWidth') ?? LIBRARY_WIDTH_DEFAULT),
      inputDevice: params.get('inputDevice') ?? '',
      silenceSplitMs: Number(params.get('silenceSplitMs') ?? '0'),
      keepSegments: params.get('keepSegments') === '1',
      learnedVocabulary: params.get('learnedVocabulary') ?? DEFAULT_LEARNED_VOCABULARY,
      modelFolder: params.get('modelFolder') ?? DEFAULT_MODEL_FOLDER,
      // A URL query decodes '+' as a space; a shortcut never holds one.
      toggleShortcut: storedShortcut((params.get('toggleShortcut') ?? '').replaceAll(' ', '+')),
    };
    const inputs = params.get('inputs');
    this.inputs =
      inputs === 'none'
        ? { defaultDevice: null, others: [] }
        : {
            defaultDevice: MOCK_DEFAULT_INPUT,
            others:
              inputs === 'many'
                ? Array.from({ length: 30 }, (_, k) => `Microphone ${String(k + 1).padStart(2, '0')}`)
                : inputs === 'long'
                  ? ['L'.repeat(80)]
                  : [...MOCK_INPUTS],
          };
    const pick = params.get('pick') ?? MOCK_PICK;
    this.pick = pick === 'cancel' ? null : pick;
    this.picked = params.get('picked') ?? null;
    this.entries = Array.from({ length: Number(params.get('library') ?? '0') }, (_, k) => preloaded(k));
    const left = Number(params.get('leftover') ?? '0');
    if (left > 0) {
      this.slots = Array.from({ length: left }, () => ({ state: 'failed', reason: 'interrupted' }));
      this.phase = 'failed';
      this.error = leftover(left);
    }
    // The core's launch: a missing model starts downloading at once.
    if (!this.modelPresent) void this.download();
  }

  get handle(): MockHandle {
    const listings = (): number => this.modelListings;
    const inputListings = (): number => this.inputListings;
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
      acts: this.acts,
      saved: this.saved,
      opened: this.opened,
      browsed: this.browsed,
      setFullscreen: (on) => {
        this.fullscreenOn = on;
        for (const cb of this.fullscreenListeners) cb(on);
      },
      get modelListings() {
        return listings();
      },
      get inputListings() {
        return inputListings();
      },
      setInputs: (others) => {
        this.inputs = { ...this.inputs, others: [...others] };
      },
      split: () => {
        this.split();
      },
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
      modelError: this.modelError(),
      download: this.received === null ? null : ({ receivedBytes: this.received, totalBytes: MODEL_BYTES } satisfies Download),
      libraryRevision: this.libraryRevision,
      microphone: this.microphone,
      learned: [...this.learned],
      partial: join(this.slots.map((s) => (s.state === 'heard' ? s.text : null))),
      rawText: this.phase === 'finished' ? this.raw : null,
      postProcessing: this.phase === 'transcribing' && this.cleaning,
    };
  }

  /** The core's model_error: why the model is missing, unless it is downloading. */
  private modelError(): string | null {
    if (this.modelPresent || this.received !== null) return null;
    return this.downloadError === null ? modelMissing(`${this.resolved().modelFolder}/ggml-large-v3-turbo.bin`) : downloadFailed(this.downloadError);
  }

  private async gate(name: string): Promise<void> {
    await this.held.get(name)?.promise;
  }

  async state(): Promise<Snapshot> {
    await this.gate('dictation_state');
    if (this.failing === 'dictation_state') throw new Error('Mock failure: dictation_state');
    return this.snapshot();
  }

  /** Rejects when `?fail=<name>` names this call. */
  private check(name: string): void {
    if (this.failing === name) throw new Error(`Mock failure: ${name}`);
  }

  async libraryList(): Promise<LibraryEntry[]> {
    await this.gate('library_list');
    this.check('library_list');
    return [...this.entries].sort(newestFirst).map(({ id, title: t, summary }) => ({ id, title: t, summary }));
  }

  async libraryGet(id: string): Promise<string> {
    await this.gate('library_get');
    this.check('library_get');
    const entry = this.entries.find((e) => e.id === id);
    if (entry === undefined) throw new Error(NOT_IN_LIBRARY);
    return entry.transcript;
  }

  async libraryCopy(id: string): Promise<void> {
    const text = await this.libraryGet(id);
    this.check('library_copy');
    if (this.clipboardFails) throw new Error('Could not copy to the clipboard: pasteboard unavailable');
    this.copied.push(text);
  }

  async settingsGet(): Promise<Settings> {
    await this.gate('settings_get');
    this.check('settings_get');
    return this.resolved();
  }

  /** The settings as the engine answers them: the glossary always a path, the models always named. */
  private resolved(): Settings {
    return {
      ...this.settings,
      vocabulary: this.settings.vocabulary || MOCK_VOCABULARY,
      summaryModel: this.settings.summaryModel || MOCK_DEFAULT_MODEL,
      cleanupModel: this.settings.cleanupModel || MOCK_DEFAULT_MODEL,
      libraryWidth: storedWidth(this.settings.libraryWidth),
      silenceSplitMs: storedSplit(this.settings.silenceSplitMs),
      learnedVocabulary: this.settings.learnedVocabulary || DEFAULT_LEARNED_VOCABULARY,
      modelFolder: this.settings.modelFolder || DEFAULT_MODEL_FOLDER,
      toggleShortcut: storedShortcut(this.settings.toggleShortcut),
    };
  }

  async settingsSet(settings: Settings): Promise<Settings> {
    this.saved.push({ ...settings });
    await this.gate('settings_set');
    this.check('settings_set');
    // The engine's write rule: '' keeps the saved glossary, the default is saved as ''.
    const vocabulary =
      settings.vocabulary === '' ? this.settings.vocabulary : settings.vocabulary === MOCK_VOCABULARY ? '' : settings.vocabulary;
    const learnedVocabulary =
      settings.learnedVocabulary === '' ? this.settings.learnedVocabulary : settings.learnedVocabulary === DEFAULT_LEARNED_VOCABULARY ? '' : settings.learnedVocabulary;
    const modelFolder =
      settings.modelFolder === '' ? this.settings.modelFolder : settings.modelFolder === DEFAULT_MODEL_FOLDER ? '' : settings.modelFolder;
    this.settings = {
      ...settings,
      vocabulary,
      learnedVocabulary,
      modelFolder,
      summaryModel: stored(settings.summaryModel, this.settings.summaryModel),
      cleanupModel: stored(settings.cleanupModel, this.settings.cleanupModel),
      // settings.json owns the hidden key: a change made in the app never touches it.
      keepSegments: this.settings.keepSegments,
    };
    return this.resolved();
  }

  /** The installed Ollama models, or a rejection under ?ollama=down. */
  async ollamaModels(): Promise<string[]> {
    this.modelListings += 1;
    await this.gate('ollama_models');
    if (this.ollamaDown) throw new Error(OLLAMA_DOWN);
    return [...MOCK_MODELS];
  }

  /** The input devices: the default and the others (see ?inputs= and setInputs). */
  async inputDevices(): Promise<InputDevices> {
    this.inputListings += 1;
    await this.gate('input_devices');
    this.check('input_devices');
    return { defaultDevice: this.inputs.defaultDevice, others: [...this.inputs.others] };
  }

  /** The core's device choice at Record and Resume: what opens, and the notice. */
  private openInput(): { name: string; notice: string | null } {
    const wanted = this.settings.inputDevice;
    const { defaultDevice, others } = this.inputs;
    if (wanted !== '' && (wanted === defaultDevice || others.includes(wanted))) return { name: wanted, notice: null };
    if (defaultDevice === null) throw new Error(NO_INPUT);
    return { name: defaultDevice, notice: wanted === '' ? null : inputFallback(wanted, defaultDevice) };
  }

  /** The open panel: the ?pick= file, or null when cancelled. */
  async pickVocabularyFile(current: string): Promise<string | null> {
    await this.gate('pick_vocabulary_file');
    this.browsed.push(current);
    this.check('pick_vocabulary_file');
    return this.pick;
  }

  /** The open panel for the model's folder: the ?picked= path, else /mock/Models; null when cancelled. */
  async pickModelFolder(current: string): Promise<string | null> {
    await this.gate('pick_model_folder');
    this.browsed.push(current);
    this.check('pick_model_folder');
    return this.picked === 'cancel' ? null : this.picked ?? '/mock/Models';
  }

  /** Opens the glossary in the text editor (the engine creates it first). */
  async vocabularyOpen(): Promise<void> {
    await this.gate('vocabulary_open');
    if (this.failing === 'vocabulary_open') throw new Error(OPEN_FAILED);
    this.opened.push(this.resolved().vocabulary);
  }

  /** Opens the learned list in the text editor (the engine creates it first). */
  async learnedVocabularyOpen(): Promise<void> {
    await this.gate('learned_vocabulary_open');
    if (this.failing === 'learned_vocabulary_open') throw new Error(OPEN_FAILED);
    this.opened.push(this.resolved().learnedVocabulary);
  }

  onChange(cb: (s: Snapshot) => void): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  async fullscreen(): Promise<boolean> {
    await this.gate('window_fullscreen');
    this.check('window_fullscreen');
    return this.fullscreenOn;
  }

  onFullscreen(cb: (on: boolean) => void): () => void {
    this.fullscreenListeners.add(cb);
    return () => this.fullscreenListeners.delete(cb);
  }

  onLevel(cb: (level: number) => void): () => void {
    this.levelListeners.add(cb);
    return () => this.levelListeners.delete(cb);
  }

  private announce(): void {
    const snapshot = this.snapshot();
    for (const cb of this.listeners) cb(snapshot);
  }

  /** The core's `apply`: refuse what the phase does not allow, else act and answer. */
  async act(action: Action, text: string): Promise<Snapshot> {
    this.acts.push(action);
    await this.gate('dictation_act');
    if (this.failing === action) throw new Error(`Mock failure: ${action}`);
    if (action === 'downloadModel') {
      if (this.modelPresent || this.received !== null) throw new Error(NO_DOWNLOAD);
      this.received = 0;
      this.revision += 1;
      void this.download();
      return this.snapshot();
    }
    if (!LEGAL[this.phase].includes(action)) throw new Error(refusal(action, this.phase));
    const closed: number[] = [];
    switch (action) {
      case 'new':
      case 'discard':
        this.stopLevels();
        this.reset();
        break;
      case 'record':
      case 'resume':
        if (!this.modelPresent) throw new Error(this.modelError() ?? DOWNLOADING);
        {
          const opened = this.openInput();
          this.microphone = opened.name;
          this.notice = opened.notice;
        }
        if (action === 'record') this.started ??= Date.now();
        this.splitMs = this.resolved().silenceSplitMs;
        this.since = Date.now();
        this.phase = 'recording';
        this.startLevels();
        break;
      case 'pause':
        this.notice = null;
        this.stopLevels();
        closed.push(this.close());
        this.phase = 'paused';
        break;
      case 'done':
        this.notice = null;
        this.stopLevels();
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
        this.learnFrom(text);
        this.text = text;
        this.notice = COPIED;
        break;
      case 'accept':
        this.learnFrom(text);
        this.text = text;
        break;
      case 'undoLearn':
        if (this.undo !== null) {
          this.learnedList = this.undo;
          this.learned = [];
          this.undo = null;
        }
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
    this.splitMs = 0;
    this.text = '';
    this.raw = null;
    this.cleaning = false;
    this.error = null;
    this.notice = null;
    this.started = null;
    this.microphone = null;
    this.learned = [];
    this.undo = null;
    this.phase = 'ready';
  }

  /** The core's learn_from: learns the corrections from the shown transcript to `edited`. */
  private learnFrom(edited: string): void {
    const found = learnCandidates(this.text, edited);
    if (found.length === 0 || found.length > MAX_RUNS) return;
    const [next, added] = rememberTerms(this.learnedList, learnTerms(this.text, edited), []);
    if (added.length === 0) return;
    this.undo = this.learnedList;
    this.learnedList = next;
    this.learned = added;
  }

  /** The core's split: the fake microphone closed the running part at a silence, and recording goes on. */
  private split(): void {
    if (this.phase !== 'recording' || this.splitMs === 0) return;
    const index = this.close();
    this.since = Date.now();
    this.revision += 1;
    void this.transcribe(index);
    this.announce();
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
    this.announce();
  }

  /** The fake microphone: a level every LEVEL_MS while recording. */
  private startLevels(): void {
    this.levelTimer = setInterval(() => {
      const level = syntheticLevel(this.levelCount);
      this.levelCount += 1;
      for (const cb of this.levelListeners) cb(level);
    }, LEVEL_MS);
  }

  private stopLevels(): void {
    if (this.levelTimer !== null) clearInterval(this.levelTimer);
    this.levelTimer = null;
  }

  /** The core's download job: progress each step, then the model is in place (or, once, a failure). */
  private async download(): Promise<void> {
    this.downloads += 1;
    const attempt = this.downloads;
    this.received = 0;
    for (let step = 1; step <= DOWNLOAD_STEPS; step++) {
      await this.gate('download');
      await new Promise((resolve) => setTimeout(resolve, DOWNLOAD_STEP_MS));
      if (this.downloadFails && attempt === 1 && step === 3) {
        this.received = null;
        this.downloadError = DOWNLOAD_REASON;
        this.revision += 1;
        this.announce();
        return;
      }
      this.received = Math.floor((MODEL_BYTES * step) / DOWNLOAD_STEPS);
      this.revision += 1;
      this.announce();
    }
    this.received = null;
    this.downloadError = null;
    this.modelPresent = true;
    this.revision += 1;
    this.announce();
  }

  /** The core's settle: wait for pending, fail on the first failure, clean up when on, else finish and copy. */
  private settle(): void {
    if (this.slots.some((s) => s.state === 'pending')) return;
    const failed = this.slots.findIndex((s) => s.state === 'failed');
    const slot = this.slots[failed];
    if (slot?.state === 'failed') {
      this.error = partFailed(failed + 1, this.slots.length, slot.reason);
      this.phase = 'failed';
      return;
    }
    const text = join(this.slots.map((s) => (s.state === 'heard' ? s.text : null)));
    if (text !== '' && this.settings.cleanup) {
      this.cleaning = true;
      void this.cleanup(this.session, text);
      return;
    }
    this.finish(text);
  }

  /** The cleanup job: the mock cleaner answers `text` unchanged, once 'cleanup' is not held. */
  private async cleanup(session: number, text: string): Promise<void> {
    await Promise.resolve();
    await this.gate('cleanup');
    if (session !== this.session || this.phase !== 'transcribing') return;
    this.finish(text);
    this.revision += 1;
    this.announce();
  }

  /** The core's finish: keep the transcript, copy it, and archive it. */
  private finish(text: string): void {
    this.raw = text;
    this.cleaning = false;
    this.text = enforceTerms(text, this.learnedList);
    this.phase = 'finished';
    if (this.text === '') this.notice = NOTHING_HEARD;
    else if (this.clipboardFails) this.notice = 'Could not copy to the clipboard: pasteboard unavailable. Use Copy to try again.';
    else {
      this.copied.push(this.text);
      this.notice = COPIED;
    }
    if (this.text !== '') this.archive();
  }

  /** The core's archive: a new entry titled by the session's start, with the excerpt; then the summary. */
  private archive(): void {
    const t = title(new Date(this.started ?? Date.now()));
    const base = baseId(t);
    let id = base;
    for (let n = 2; this.entries.some((e) => e.id === id); n++) id = `${base}-${String(n)}`;
    this.entries.push({ id, title: t, summary: excerpt(this.text), transcript: this.text });
    this.libraryRevision += 1;
    this.revision += 1;
    if (this.settings.summaries) void this.summarize(id);
  }

  /** The summary job: off the transcription path, keyed by id. */
  private async summarize(id: string): Promise<void> {
    await Promise.resolve();
    await this.gate('summarize');
    const entry = this.entries.find((e) => e.id === id);
    if (this.ollamaDown || entry === undefined) return;
    entry.summary = MOCK_SUMMARY;
    this.libraryRevision += 1;
    this.revision += 1;
    this.announce();
  }
}
