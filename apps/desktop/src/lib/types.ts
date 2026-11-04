// The bridge contract's JSON shapes (docs/design.md). The Rust side is
// crates/scriber-core/src/model.rs; crates/scriber-core/tests/fixtures/bridge-shapes.json
// holds one example of each, checked against both sides.

export type Phase = 'idle' | 'ready' | 'recording' | 'paused' | 'transcribing' | 'finished' | 'failed';

export type Action = 'new' | 'record' | 'pause' | 'resume' | 'done' | 'discard' | 'retry' | 'copy' | 'accept' | 'downloadModel' | 'undoLearn';

/** How far the speech model's download has got. */
export interface Download {
  receivedBytes: number;
  totalBytes: number;
}

export interface Snapshot {
  /** Grows with every change; the window keeps the highest it has seen. */
  revision: number;
  phase: Phase;
  /** Milliseconds of audio in the closed segments; the window adds the running one. */
  recordedMs: number;
  /** Segments recorded in this session. */
  segments: number;
  /** Of those, how many are still being transcribed. */
  pending: number;
  /** The transcript once finished; empty before. */
  text: string;
  /** Why the session failed, when the phase is `failed`. */
  error: string | null;
  /** One sentence about the last action. */
  notice: string | null;
  /** Set when the speech model is missing and not downloading; the window offers `downloadModel`. */
  modelError: string | null;
  /** The speech model's download, while it runs. */
  download: Download | null;
  /** Grows whenever the recording library gains an entry or a summary; the window lists it again. */
  libraryRevision: number;
  /** The input device opened for this session's latest segment, by name; null before the first Record. */
  microphone: string | null;
  /** The terms the last Copy learned from the user's corrections, until Undo, New, Discard or relaunch; [] when none. */
  learned: string[];
  /** The heard segments' raw Whisper text so far, joined in recording order; '' for a new session. */
  partial: string;
  /** Finished only: Whisper's joined transcript before cleanup, glyphs and learned spellings; null before finish and after a relaunch. */
  rawText: string | null;
}

/** One recording in the library, as the drawer lists it. */
export interface LibraryEntry {
  /** The entry's folder name: its start, `20261026-140309`, with `-2` and so on after a collision. */
  id: string;
  /** Its start in local time: `2026-10-26 14:03:09`. */
  title: string;
  /** One sentence: the local model's summary, or an excerpt of the transcript. */
  summary: string;
}

/** The user's settings. */
export interface Settings {
  /** Whether new recordings are summarized by the local model (Ollama). */
  summaries: boolean;
  /** Whether the library drawer is open. */
  libraryOpen: boolean;
  /** The glossary file's absolute path; send '' when it is not known yet (the saved one is kept). */
  vocabulary: string;
  /** Whether each transcript is cleaned up by the local model (Ollama). */
  cleanup: boolean;
  /** The Ollama model for summaries. */
  summaryModel: string;
  /** The Ollama model for post-processing. */
  cleanupModel: string;
  /** The library drawer's width in px, a whole number from 110 to 440 (a fraction is refused by the bridge). */
  libraryWidth: number;
  /** The input device to record from, by name; '' means the Mac default. Saved and answered verbatim. */
  inputDevice: string;
  /** Close the running part after this many ms of silence while recording; 0 is off (the window offers 0, 1500, 2500, 4000). */
  silenceSplitMs: number;
  /** A hidden setting: no UI, set by editing settings.json. Keeps each part's exact WAV in the library entry. */
  keepSegments: boolean;
  /** The learned list's absolute path; send '' when it is not known yet (the saved one is kept). */
  learnedVocabulary: string;
}

/** The input devices the microphone menu offers. */
export interface InputDevices {
  /** The Mac's default input device, by name; null when there is none. */
  defaultDevice: string | null;
  /** Every other input device's name, sorted case-insensitively. */
  others: string[];
}
