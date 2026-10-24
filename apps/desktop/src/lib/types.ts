// The bridge contract's JSON shapes (docs/design.md). The Rust side is
// crates/scriber-core/src/model.rs; crates/scriber-core/tests/fixtures/bridge-shapes.json
// holds one example of each, checked against both sides.

export type Phase = 'idle' | 'ready' | 'recording' | 'paused' | 'transcribing' | 'finished' | 'failed';

export type Action = 'new' | 'record' | 'pause' | 'resume' | 'done' | 'discard' | 'retry' | 'copy';

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
  /** Set when the speech model is not installed. */
  modelError: string | null;
}
