// WebSocket backend for Scriber's web build (docs/design-web.md).
// Imported only by bridge.ts when VITE_SCRIBER_BACKEND=web. Speaks the
// documented /ws protocol, coordinates gesture-started microphone capture and
// clipboard writes, and projects web-local clipboard/reconnect feedback without
// mutating raw Snapshots.

import {
  BrowserCapture,
  DEFAULT_DRAIN_TIMEOUT_MS,
  type CaptureObservation,
} from './browser-capture';
import type { Action, InputDevices, LibraryEntry, Phase, Settings, Snapshot } from './types';

export const WEB_ENDPOINT_KEY = 'scriber.web.endpoint';
export const WEB_TOKEN_KEY = 'scriber.web.token';

export const DEFAULT_HELLO_TIMEOUT_MS = 5_000;
export const DEFAULT_RECONNECT_DELAY_MS = 1_000;
export const DEFAULT_RPC_TIMEOUT_MS = 15_000;
export const DEFAULT_CLIPBOARD_TIMEOUT_MS = 300_000;

export const COPIED_PREFIX = 'Copied to the clipboard.';
export const CONNECTION_LOST_NOTICE = 'Connection lost. Press Done to finish or Discard to start over.';

const MAX_OBSERVATIONS = 200;
const CLOSE_UNAUTHORIZED = 4401;
const CLOSE_MESSAGE_TOO_BIG = 1009;

export interface WebConnectionConfig {
  endpoint: string;
  token: string;
}

export interface WebTimeouts {
  helloMs: number;
  reconnectMs: number;
  rpcMs: number;
  drainMs: number;
  clipboardMs: number;
}

export interface WebObservation {
  seq: number;
  category: 'connection' | 'rpc' | 'capture' | 'clipboard';
  event: string;
  outcome: 'ok' | 'skip' | 'refused' | 'timeout' | 'cancelled' | 'failed';
  reason: string;
  generation: number;
  requestId?: number;
  phase?: Phase;
  revision?: number;
  frames?: number;
  samples?: number;
  bytes?: number;
}

export type WebClipboardOutcome =
  | { kind: 'unwritten' }
  | { kind: 'copied' }
  | { kind: 'failed'; reason: string };

export interface WebUiState {
  connectionLostNotice: boolean;
  clipboardOutcome: WebClipboardOutcome;
  archiveSuffix: string | null;
}

export const INITIAL_WEB_UI_STATE: WebUiState = {
  connectionLostNotice: false,
  clipboardOutcome: { kind: 'unwritten' },
  archiveSuffix: null,
};

/** Distinguishes browser clipboard failure from post-copy learning RPC failure. */
export class WebCopyError extends Error {
  readonly stage: 'clipboard' | 'learning';

  constructor(stage: 'clipboard' | 'learning', message: string) {
    super(message);
    this.name = 'WebCopyError';
    this.stage = stage;
  }
}

class InternalCancelError extends Error {
  constructor(reason: string) {
    super(reason);
    this.name = 'InternalCancelError';
  }
}

interface ScriberWebTestHook {
  timeouts?: Partial<WebTimeouts>;
  observations?: WebObservation[];
  observe?: (entry: WebObservation) => void;
  bridge?: Record<string, unknown>;
}

declare global {
  interface Window {
    __scriberWebTest?: ScriberWebTestHook;
  }
}

interface PendingRpc {
  generation: number;
  id: number;
  timer: ReturnType<typeof setTimeout>;
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
}

interface ReadyWaiter {
  resolve: () => void;
  reject: (err: Error) => void;
}

interface WireInputDevices {
  default: string | null;
  others: string[];
}

interface DoneAssociation {
  readonly id: number;
  readonly generation: number;
  requestId: number | undefined;
  state: 'awaiting_reply' | 'tracking' | 'resolved' | 'cancelled';
  buffered: Snapshot[];
  lastSnap: Snapshot | null;
  seenByRevision: Map<number, Snapshot>;
  matchedText: string | null;
  earlyClipboardError: string | null;
  settledBlob: boolean;
  timer: ReturnType<typeof setTimeout> | null;
  resolveBlob: (blob: Blob) => void;
  rejectBlob: (err: Error) => void;
}

function clearTimer(timer: ReturnType<typeof setTimeout> | null): null {
  if (timer !== null) clearTimeout(timer);
  return null;
}

function toError(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

function readableError(err: unknown): string {
  return toError(err).message;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isValidHelloResponse(msg: unknown): boolean {
  return isRecord(msg) && isRecord(msg.hello) && typeof msg.hello.version === 'string' && msg.hello.platform === 'web';
}

function snapshotsEqual(a: Snapshot, b: Snapshot): boolean {
  return (
    a.revision === b.revision &&
    a.phase === b.phase &&
    a.recordedMs === b.recordedMs &&
    a.segments === b.segments &&
    a.pending === b.pending &&
    a.text === b.text &&
    a.error === b.error &&
    a.notice === b.notice &&
    a.modelError === b.modelError &&
    a.libraryRevision === b.libraryRevision &&
    a.microphone === b.microphone &&
    a.partial === b.partial &&
    a.rawText === b.rawText &&
    a.postProcessing === b.postProcessing &&
    JSON.stringify(a.download) === JSON.stringify(b.download) &&
    JSON.stringify(a.learned) === JSON.stringify(b.learned)
  );
}

function extractArchiveSuffix(notice: string | null): string | null {
  if (notice !== null && notice.startsWith(`${COPIED_PREFIX} `)) {
    return notice.slice(COPIED_PREFIX.length + 1);
  }
  return null;
}

/** Projects web-local clipboard and former-owner reconnect notices onto a raw Snapshot without mutating it. */
export function projectWebSnapshot(raw: Snapshot, ui: WebUiState): Snapshot {
  if (raw.phase === 'recording' && ui.connectionLostNotice) {
    return { ...raw, notice: CONNECTION_LOST_NOTICE };
  }
  if (raw.phase !== 'finished' || raw.text === '') {
    return raw;
  }
  const suffix = extractArchiveSuffix(raw.notice) ?? ui.archiveSuffix;
  switch (ui.clipboardOutcome.kind) {
    case 'unwritten':
      return raw.notice === COPIED_PREFIX
        ? { ...raw, notice: null }
        : suffix !== null
          ? { ...raw, notice: suffix }
          : raw;
    case 'copied': {
      const notice = suffix !== null ? `${COPIED_PREFIX} ${suffix}` : COPIED_PREFIX;
      return { ...raw, notice };
    }
    case 'failed': {
      const base = `Could not copy to the clipboard: ${ui.clipboardOutcome.reason}. Use Copy to try again.`;
      const notice = suffix !== null ? `${base} ${suffix}` : base;
      return { ...raw, notice };
    }
  }
}

function normalizeOrigin(raw: string): string {
  const trimmed = raw.trim();
  const base = window.location.origin;
  if (trimmed === '') return base;
  const parsed = new URL(trimmed, base);
  if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
    return `${parsed.protocol}//${parsed.host}`;
  }
  if (parsed.protocol === 'ws:' || parsed.protocol === 'wss:') {
    return `${parsed.protocol === 'wss:' ? 'https:' : 'http:'}//${parsed.host}`;
  }
  throw new Error('Server URL must use http, https, ws or wss.');
}

function wsUrlFromOrigin(origin: string): string {
  const parsed = new URL(origin);
  return `${parsed.protocol === 'https:' ? 'wss:' : 'ws:'}//${parsed.host}/ws`;
}

function startClipboardItemWrite(blobPromise: Promise<Blob>): Promise<void> {
  if (
    typeof navigator === 'undefined' ||
    !('clipboard' in navigator) ||
    typeof navigator.clipboard.write !== 'function' ||
    typeof ClipboardItem === 'undefined'
  ) {
    return Promise.reject(new Error('clipboard unavailable'));
  }
  try {
    const item = new ClipboardItem({ 'text/plain': blobPromise });
    return navigator.clipboard.write([item]);
  } catch (err) {
    return Promise.reject(toError(err));
  }
}

function startClipboardBlobWrite(text: string): Promise<void> {
  return startClipboardItemWrite(Promise.resolve(new Blob([text], { type: 'text/plain' })));
}

export class WebBridge {
  private readonly capture = new BrowserCapture();
  private memEndpoint: string;
  private memToken: string;
  private ws: WebSocket | null = null;
  private authenticated = false;
  private generation = 0;
  private hasAuthenticatedOnce = false;
  private authRefused = false;
  private lastConnectionError: string | null = null;
  private helloTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private nextRpcId = 0;
  private readonly pendingRpcs = new Map<number, PendingRpc>();
  private readyWaiters: ReadyWaiter[] = [];

  private readonly dictationListeners = new Set<(snapshot: Snapshot) => void>();
  private readonly levelListeners = new Set<(level: number) => void>();
  private readonly rebindListeners = new Set<(generation: number) => void>();
  private readonly uiListeners = new Set<(state: WebUiState) => void>();

  private ownedRecording = false;
  private wasOwnerBeforeDisconnect = false;
  private connectionLostNotice = false;
  private lastSnapshotRevision: number | null = null;

  private finishAnchored = false;
  private finishText: string | null = null;
  private clipboardOutcome: WebClipboardOutcome = { kind: 'unwritten' };
  private archiveSuffix: string | null = null;
  private nextAssocId = 0;
  private activeDoneAssoc: DoneAssociation | null = null;
  private obsSeq = 0;

  constructor() {
    this.memEndpoint = this.readStoredEndpoint();
    this.memToken = this.readStoredToken();
  }

  /** Marks <html> with data-bridge="web" and opens the initial socket. */
  identify(): void {
    document.documentElement.dataset.bridge = 'web';
    this.ensureConnected();
  }

  /** Stores endpoint origin and token (with in-memory fallback) and immediately rebinds. */
  configureConnection(config: WebConnectionConfig): void {
    const origin = normalizeOrigin(config.endpoint);
    this.memEndpoint = origin;
    this.memToken = config.token;
    this.writeStorage(WEB_ENDPOINT_KEY, origin);
    this.writeStorage(WEB_TOKEN_KEY, config.token);
    this.authRefused = false;
    this.retire('configure', 'Reconnecting with updated server settings.');
    this.connect('configure');
  }

  /** Explicitly retries the connection (clears an auth-refused latch). */
  retryConnection(): void {
    if (this.ws !== null) return;
    this.authRefused = false;
    this.retire('explicit_retry', 'Reconnecting to the Scriber server.');
    this.connect('explicit_retry');
  }

  /** Current projected web UI feedback state. */
  uiState(): WebUiState {
    return {
      connectionLostNotice: this.connectionLostNotice,
      clipboardOutcome: this.clipboardOutcome,
      archiveSuffix: this.archiveSuffix,
    };
  }

  onUiStateChanged(cb: (state: WebUiState) => void): void {
    this.uiListeners.add(cb);
    cb(this.uiState());
  }

  onRebind(cb: (generation: number) => void): void {
    this.rebindListeners.add(cb);
  }

  onDictationChanged(cb: (snapshot: Snapshot) => void): () => void {
    this.dictationListeners.add(cb);
    return () => {
      this.dictationListeners.delete(cb);
    };
  }

  onLevel(cb: (level: number) => void): () => void {
    this.levelListeners.add(cb);
    return () => {
      this.levelListeners.delete(cb);
    };
  }

  onFullscreenChanged(): () => void {
    return () => undefined;
  }

  async appVersion(): Promise<string> {
    return (await this.callWithId<string>('app_version', {})).promise;
  }

  async dictationState(): Promise<Snapshot> {
    const snap = await (await this.callWithId<Snapshot>('dictation_state', {})).promise;
    this.onSnapshot(snap);
    return snap;
  }

  async act(action: Action, text = ''): Promise<Snapshot> {
    if (action === 'record' || action === 'resume') {
      return this.actStartCapture(action);
    }
    if (action === 'pause') {
      return this.actPause();
    }
    if (action === 'done') {
      return this.actDone();
    }
    if (action === 'discard') {
      return this.actDiscard();
    }
    if (action === 'copy') {
      return this.actCopy(text);
    }
    const snap = await (await this.callWithId<Snapshot>('dictation_act', { action, text })).promise;
    this.onSnapshot(snap);
    return snap;
  }

  async libraryList(): Promise<LibraryEntry[]> {
    return (await this.callWithId<LibraryEntry[]>('library_list', {})).promise;
  }

  async libraryGet(id: string): Promise<string> {
    return (await this.callWithId<string>('library_get', { id })).promise;
  }

  /**
   * Client special case: starts navigator.clipboard.write synchronously inside
   * the click stack with a promised Blob populated by library_get (never calls
   * library_copy over the wire).
   */
  async libraryCopy(id: string): Promise<void> {
    const gen = this.generation;
    const state: { requestId: number | undefined; fetchError: Error | null } = {
      requestId: undefined,
      fetchError: null,
    };
    const blobPromise = this.callWithId<string>('library_get', { id })
      .then((handle) => {
        state.requestId = handle.requestId;
        return handle.promise;
      })
      .then(
        (transcript) => new Blob([transcript], { type: 'text/plain' }),
        (err: unknown) => {
          state.fetchError = toError(err);
          throw state.fetchError;
        },
      );
    // Prevent unhandled rejection if clipboard.write fails synchronously before awaiting blobPromise.
    blobPromise.catch(() => undefined);

    const writePromise = startClipboardItemWrite(blobPromise);
    try {
      await writePromise;
      this.observe({ category: 'clipboard', event: 'library_copy', outcome: 'ok', reason: 'written', generation: gen, requestId: state.requestId });
    } catch (err) {
      if (state.fetchError !== null) {
        this.observe({ category: 'clipboard', event: 'library_copy', outcome: 'failed', reason: 'missing_entry', generation: gen, requestId: state.requestId });
        throw state.fetchError;
      }
      const reason = readableError(err);
      this.observe({ category: 'clipboard', event: 'library_copy', outcome: 'failed', reason: 'write_error', generation: gen, requestId: state.requestId });
      throw new Error(`Could not copy to the clipboard: ${reason}. Use Copy to try again.`);
    }
  }

  async settingsGet(): Promise<Settings> {
    return (await this.callWithId<Settings>('settings_get', {})).promise;
  }

  async settingsSet(settings: Settings): Promise<Settings> {
    return (await this.callWithId<Settings>('settings_set', { settings })).promise;
  }

  async ollamaModels(): Promise<string[]> {
    return (await this.callWithId<string[]>('ollama_models', {})).promise;
  }

  async inputDevices(): Promise<InputDevices> {
    const wire = await (await this.callWithId<WireInputDevices>('input_devices', {})).promise;
    return { defaultDevice: wire.default, others: wire.others };
  }

  async windowFullscreen(): Promise<boolean> {
    return (await this.callWithId<boolean>('window_fullscreen', {})).promise;
  }

  async vocabularyOpen(): Promise<void> {
    await (await this.callWithId<unknown>('vocabulary_open', {})).promise;
  }

  async learnedVocabularyOpen(): Promise<void> {
    await (await this.callWithId<unknown>('learned_vocabulary_open', {})).promise;
  }

  async pickVocabularyFile(): Promise<string | null> {
    return (await this.callWithId<string | null>('pick_vocabulary_file', {})).promise;
  }

  async pickModelFolder(): Promise<string | null> {
    return (await this.callWithId<string | null>('pick_model_folder', {})).promise;
  }

  // ---- Action coordinators ---------------------------------------------------

  private async actStartCapture(action: 'record' | 'resume'): Promise<Snapshot> {
    const gen = this.generation;
    const expectedPrePhase = action === 'record' ? 'ready' : 'paused';
    const handle = await this.capture.startGesture(gen, expectedPrePhase, {
      sendFrame: (frame) => {
        this.sendBinary(gen, frame);
      },
      observe: (entry) => {
        this.observeCapture(entry);
      },
    });

    let snap: Snapshot;
    try {
      snap = await (await this.callWithId<Snapshot>('dictation_act', { action, text: '' })).promise;
    } catch (err) {
      handle.cancel('action_rejected');
      throw err;
    }

    if (gen !== this.generation || snap.phase !== 'recording') {
      handle.cancel('action_rejected');
      this.onSnapshot(snap);
      return snap;
    }

    if (handle.arm()) {
      this.ownedRecording = true;
      this.wasOwnerBeforeDisconnect = false;
      this.connectionLostNotice = false;
      this.notifyUi();
    }
    this.onSnapshot(snap);
    return snap;
  }

  private async actPause(): Promise<Snapshot> {
    const gen = this.generation;
    if (this.capture.isActive()) {
      await this.capture.drainAndStop(gen, this.timeouts().drainMs);
    } else {
      this.observe({
        category: 'capture',
        event: 'drain',
        outcome: 'skip',
        reason: 'not_active',
        generation: gen,
      });
    }
    this.ownedRecording = false;
    const snap = await (await this.callWithId<Snapshot>('dictation_act', { action: 'pause', text: '' })).promise;
    this.onSnapshot(snap);
    return snap;
  }

  private async actDiscard(): Promise<Snapshot> {
    this.capture.stop('discard');
    this.ownedRecording = false;
    this.wasOwnerBeforeDisconnect = false;
    this.connectionLostNotice = false;
    this.cancelDoneAssociation('superseding_phase');
    this.notifyUi();
    const snap = await (await this.callWithId<Snapshot>('dictation_act', { action: 'discard', text: '' })).promise;
    this.onSnapshot(snap);
    return snap;
  }

  private async actDone(): Promise<Snapshot> {
    const gen = this.generation;
    // Start clipboard.write synchronously inside the gesture before any await.
    const assoc = this.startDoneAssociation(gen);

    if (this.capture.isActive()) {
      try {
        await this.capture.drainAndStop(gen, this.timeouts().drainMs);
      } catch (err) {
        this.cancelDoneAssociation('drain_failed', assoc);
        throw err;
      }
    } else {
      this.observe({
        category: 'capture',
        event: 'drain',
        outcome: 'skip',
        reason: 'not_active',
        generation: gen,
      });
    }

    this.ownedRecording = false;
    let snap: Snapshot;
    try {
      const handle = await this.callWithId<Snapshot>('dictation_act', { action: 'done', text: '' });
      assoc.requestId = handle.requestId;
      snap = await handle.promise;
    } catch (err) {
      this.cancelDoneAssociation('done_rejected', assoc);
      throw err;
    }

    this.onDoneReply(assoc, snap);
    this.onSnapshot(snap);
    return snap;
  }

  private async actCopy(text: string): Promise<Snapshot> {
    const gen = this.generation;
    // Start browser clipboard write synchronously inside the click gesture.
    const writePromise = startClipboardBlobWrite(text);
    try {
      await writePromise;
    } catch (err) {
      const reason = readableError(err);
      this.clipboardOutcome = { kind: 'failed', reason };
      this.notifyUi();
      this.observe({
        category: 'clipboard',
        event: 'copy',
        outcome: 'failed',
        reason: 'write_error',
        generation: gen,
      });
      throw new WebCopyError('clipboard', reason);
    }

    this.clipboardOutcome = { kind: 'copied' };
    this.notifyUi();

    let requestId: number | undefined;
    try {
      const handle = await this.callWithId<Snapshot>('dictation_act', { action: 'copy', text });
      requestId = handle.requestId;
      const snap = await handle.promise;
      this.observe({
        category: 'clipboard',
        event: 'copy',
        outcome: 'ok',
        reason: 'written',
        generation: gen,
        requestId,
        phase: snap.phase,
        revision: snap.revision,
      });
      this.onSnapshot(snap);
      return snap;
    } catch (err) {
      const reason = readableError(err);
      this.observe({ category: 'clipboard', event: 'copy', outcome: 'failed', reason: 'learning_rpc_error', generation: gen, requestId });
      throw new WebCopyError('learning', reason);
    }
  }

  // ---- Done clipboard association -------------------------------------------

  private startDoneAssociation(generation: number): DoneAssociation {
    this.cancelDoneAssociation('superseded');

    let resolveBlob!: (blob: Blob) => void;
    let rejectBlob!: (err: Error) => void;
    const blobPromise = new Promise<Blob>((resolve, reject) => {
      resolveBlob = resolve;
      rejectBlob = reject;
    });
    blobPromise.catch(() => undefined);

    const assoc: DoneAssociation = {
      id: ++this.nextAssocId,
      generation,
      requestId: undefined,
      state: 'awaiting_reply',
      buffered: [],
      lastSnap: null,
      seenByRevision: new Map(),
      matchedText: null,
      earlyClipboardError: null,
      settledBlob: false,
      timer: null,
      resolveBlob,
      rejectBlob,
    };
    this.activeDoneAssoc = assoc;

    assoc.timer = setTimeout(() => {
      this.cancelDoneAssociation('association_timeout', assoc, 'timeout');
    }, this.timeouts().clipboardMs);

    startClipboardItemWrite(blobPromise).then(
      () => {
        if (this.finishAnchored && this.finishText === assoc.matchedText) {
          this.clipboardOutcome = { kind: 'copied' };
          this.notifyUi();
        }
        this.observe({
          category: 'clipboard',
          event: 'done',
          outcome: 'ok',
          reason: 'written',
          generation: assoc.generation,
          requestId: assoc.requestId,
          phase: 'finished',
          revision: assoc.lastSnap?.revision,
        });
      },
      (err: unknown) => {
        if (err instanceof InternalCancelError) return;
        const reason = readableError(err);
        if (assoc.state === 'resolved' && assoc.matchedText !== null) {
          if (this.finishAnchored && this.finishText === assoc.matchedText) {
            this.clipboardOutcome = { kind: 'failed', reason };
            this.notifyUi();
          }
          this.observe({
            category: 'clipboard',
            event: 'done',
            outcome: 'failed',
            reason: 'late_write_error',
            generation: assoc.generation,
            requestId: assoc.requestId,
            phase: 'finished',
            revision: assoc.lastSnap?.revision,
          });
          return;
        }
        // Early write rejection before Done finishes: record for when the local finish arrives.
        assoc.earlyClipboardError = reason;
      },
    );

    return assoc;
  }

  private onDoneReply(assoc: DoneAssociation, reply: Snapshot): void {
    if (reply.phase === 'finished') {
      if (reply.text === '') {
        this.cancelDoneAssociation('empty_text', assoc, 'cancelled', reply);
        return;
      }
      this.resolveDoneAssociation(assoc, reply);
      return;
    }

    if (reply.phase !== 'transcribing') {
      this.cancelDoneAssociation('session_failed', assoc, 'cancelled', reply);
      return;
    }

    assoc.state = 'tracking';
    assoc.lastSnap = reply;
    assoc.seenByRevision.set(reply.revision, reply);
    const queued = assoc.buffered;
    assoc.buffered = [];
    for (const ev of queued) {
      if (this.isTrackingAssoc(assoc)) this.advanceDoneAssociation(assoc, ev);
    }
  }

  private isTrackingAssoc(assoc: DoneAssociation): boolean {
    return assoc.state === 'tracking';
  }

  private advanceDoneAssociation(assoc: DoneAssociation, snap: Snapshot): void {
    const last = assoc.lastSnap;
    if (last === null) return;

    const existing = assoc.seenByRevision.get(snap.revision);
    if (existing !== undefined) {
      if (!snapshotsEqual(existing, snap)) {
        this.cancelDoneAssociation('contradictory_revision', assoc, 'cancelled', snap);
      }
      return;
    }

    if (snap.revision < last.revision) return;

    const deltaRev = snap.revision - last.revision;
    const isAtomicArchiveFinish =
      deltaRev === 2 &&
      snap.phase === 'finished' &&
      snap.text !== '' &&
      snap.libraryRevision === last.libraryRevision + 1;

    if (deltaRev !== 1 && !isAtomicArchiveFinish) {
      this.cancelDoneAssociation('revision_gap', assoc, 'cancelled', snap);
      return;
    }

    assoc.seenByRevision.set(snap.revision, snap);
    assoc.lastSnap = snap;

    if (snap.phase === 'finished') {
      if (snap.text === '') {
        this.cancelDoneAssociation('empty_text', assoc, 'cancelled', snap);
      } else {
        this.resolveDoneAssociation(assoc, snap);
      }
    } else if (snap.phase !== 'transcribing') {
      const reason = snap.phase === 'failed' ? 'session_failed' : 'superseding_phase';
      this.cancelDoneAssociation(reason, assoc, 'cancelled', snap);
    }
  }

  private resolveDoneAssociation(assoc: DoneAssociation, snap: Snapshot): void {
    assoc.settledBlob = true;
    assoc.state = 'resolved';
    assoc.lastSnap = snap;
    assoc.matchedText = snap.text;
    assoc.timer = clearTimer(assoc.timer);
    if (this.activeDoneAssoc === assoc) this.activeDoneAssoc = null;

    this.finishAnchored = true;
    this.finishText = snap.text;
    this.archiveSuffix = extractArchiveSuffix(snap.notice);

    if (assoc.earlyClipboardError !== null) {
      this.clipboardOutcome = { kind: 'failed', reason: assoc.earlyClipboardError };
      this.notifyUi();
      this.observe({
        category: 'clipboard',
        event: 'done',
        outcome: 'failed',
        reason: 'early_write_error',
        generation: assoc.generation,
        requestId: assoc.requestId,
        phase: snap.phase,
        revision: snap.revision,
      });
    } else {
      this.clipboardOutcome = { kind: 'unwritten' };
      this.notifyUi();
    }

    assoc.resolveBlob(new Blob([snap.text], { type: 'text/plain' }));
  }

  private cancelDoneAssociation(
    reason: string,
    target?: DoneAssociation,
    outcome: 'cancelled' | 'timeout' = 'cancelled',
    snap?: Snapshot,
  ): void {
    const assoc = target ?? this.activeDoneAssoc;
    if (assoc === null || assoc.settledBlob) return;
    assoc.settledBlob = true;
    assoc.state = 'cancelled';
    assoc.timer = clearTimer(assoc.timer);
    if (this.activeDoneAssoc === assoc) this.activeDoneAssoc = null;
    this.observe({
      category: 'clipboard',
      event: 'done',
      outcome,
      reason,
      generation: assoc.generation,
      requestId: assoc.requestId,
      phase: snap?.phase,
      revision: snap?.revision,
    });
    assoc.rejectBlob(new InternalCancelError(reason));
  }

  // ---- Snapshot & event routing ---------------------------------------------

  private onSnapshotEvent(generation: number, snap: Snapshot): void {
    this.capture.handleRemotePhase(snap.phase);

    const assoc = this.activeDoneAssoc;
    if (assoc !== null && assoc.generation === generation) {
      if (assoc.state === 'awaiting_reply') {
        assoc.buffered.push(snap);
      } else if (assoc.state === 'tracking') {
        this.advanceDoneAssociation(assoc, snap);
      }
    }

    this.onSnapshot(snap);
  }

  private onSnapshot(snap: Snapshot): void {
    if (this.lastSnapshotRevision !== null && snap.revision < this.lastSnapshotRevision) {
      return;
    }
    this.lastSnapshotRevision = snap.revision;
    let changed = false;
    if (snap.phase === 'recording') {
      if (this.wasOwnerBeforeDisconnect) {
        this.wasOwnerBeforeDisconnect = false;
        this.connectionLostNotice = true;
        changed = true;
      }
    } else if (this.ownedRecording || this.wasOwnerBeforeDisconnect || this.connectionLostNotice) {
      this.ownedRecording = false;
      this.wasOwnerBeforeDisconnect = false;
      this.connectionLostNotice = false;
      changed = true;
    }

    if (snap.phase !== 'finished') {
      if (this.finishAnchored) {
        this.finishAnchored = false;
        this.finishText = null;
        this.clipboardOutcome = { kind: 'unwritten' };
        this.archiveSuffix = null;
        changed = true;
      }
    } else if (!this.finishAnchored || (this.finishText !== null && this.finishText !== snap.text)) {
      this.finishAnchored = true;
      this.finishText = snap.text;
      this.clipboardOutcome = { kind: 'unwritten' };
      this.archiveSuffix = extractArchiveSuffix(snap.notice);
      changed = true;
    }

    if (changed) this.notifyUi();
  }

  private notifyUi(): void {
    const state = this.uiState();
    for (const cb of this.uiListeners) cb(state);
  }

  // ---- WebSocket transport & lifecycle --------------------------------------

  private ensureConnected(): void {
    if (this.ws !== null || this.reconnectTimer !== null) return;
    this.connect('initial');
  }

  private connect(reason: 'initial' | 'reconnect' | 'explicit_retry' | 'configure'): void {
    this.clearReconnectTimer();

    const gen = ++this.generation;
    this.authenticated = false;
    this.lastConnectionError = null;
    this.observe({
      category: 'connection',
      event: 'connect_start',
      outcome: 'ok',
      reason,
      generation: gen,
    });

    const url = wsUrlFromOrigin(this.memEndpoint);
    const ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';
    this.ws = ws;

    this.helloTimer = setTimeout(() => {
      this.observe({
        category: 'connection',
        event: 'hello',
        outcome: 'timeout',
        reason: 'hello_timeout',
        generation: gen,
      });
      this.handleDisconnect(
        gen,
        'hello_timeout',
        'Timed out waiting for the Scriber server handshake.',
        true,
      );
    }, this.timeouts().helloMs);

    ws.onopen = () => {
      ws.send(JSON.stringify({ hello: { token: this.memToken } }));
    };

    ws.onmessage = (event: MessageEvent) => {
      let msg: unknown;
      try {
        msg = JSON.parse(String(event.data)) as unknown;
      } catch {
        if (!this.authenticated) {
          this.observe({
            category: 'connection',
            event: 'hello',
            outcome: 'failed',
            reason: 'invalid_hello',
            generation: gen,
          });
          this.handleDisconnect(
            gen,
            'invalid_hello',
            'The server did not complete the Scriber handshake.',
            true,
          );
        }
        return;
      }

      if (!this.authenticated) {
        if (!isValidHelloResponse(msg)) {
          this.observe({
            category: 'connection',
            event: 'hello',
            outcome: 'failed',
            reason: 'invalid_hello',
            generation: gen,
          });
          this.handleDisconnect(
            gen,
            'invalid_hello',
            'The server did not complete the Scriber handshake.',
            true,
          );
          return;
        }
        this.helloTimer = clearTimer(this.helloTimer);
        this.authenticated = true;
        this.lastConnectionError = null;
        this.observe({
          category: 'connection',
          event: 'hello',
          outcome: 'ok',
          reason: 'authenticated',
          generation: gen,
        });

        const shouldNotifyRebind = this.hasAuthenticatedOnce || gen > 1 || this.readyWaiters.length === 0;
        this.hasAuthenticatedOnce = true;
        this.lastSnapshotRevision = null;
        if (shouldNotifyRebind) {
          for (const cb of this.rebindListeners) cb(gen);
        }

        const waiters = this.readyWaiters;
        this.readyWaiters = [];
        for (const w of waiters) w.resolve();
        return;
      }

      this.handleAuthenticatedMessage(gen, msg);
    };

    ws.onclose = (event: CloseEvent) => {
      if (event.code === CLOSE_UNAUTHORIZED) {
        this.authRefused = true;
        this.observe({
          category: 'connection',
          event: 'hello',
          outcome: 'refused',
          reason: 'auth_4401',
          generation: gen,
        });
        this.handleDisconnect(
          gen,
          'auth_4401',
          'Authentication refused by the Scriber server. Check the token and try again.',
          false,
        );
        return;
      }
      if (event.code === CLOSE_MESSAGE_TOO_BIG) {
        this.handleDisconnect(
          gen,
          'message_too_big',
          'Connection closed: message exceeded the server size limit.',
          true,
        );
        return;
      }
      if (!this.authenticated) {
        this.observe({
          category: 'connection',
          event: 'hello',
          outcome: 'failed',
          reason: 'closed_before_hello',
          generation: gen,
        });
        this.handleDisconnect(
          gen,
          'closed_before_hello',
          'The server did not complete the Scriber handshake.',
          true,
        );
        return;
      }
      this.handleDisconnect(gen, 'disconnect', 'Connection to the Scriber server was lost.', true);
    };
  }

  private handleAuthenticatedMessage(generation: number, msg: unknown): void {
    if (!isRecord(msg)) return;
    if (typeof msg.event === 'string') {
      if (msg.event === 'dictation-changed' && isRecord(msg.payload)) {
        const snap = msg.payload as unknown as Snapshot;
        this.onSnapshotEvent(generation, snap);
        for (const cb of this.dictationListeners) cb(snap);
      } else if (msg.event === 'dictation-level' && typeof msg.payload === 'number') {
        for (const cb of this.levelListeners) cb(msg.payload);
      }
      return;
    }

    if ('id' in msg) {
      if (msg.id === null && typeof msg.err === 'string') {
        this.observe({
          category: 'rpc',
          event: 'uncorrelated',
          outcome: 'failed',
          reason: 'uncorrelated_error',
          generation,
        });
        return;
      }
      if (typeof msg.id === 'number') {
        const pending = this.pendingRpcs.get(msg.id);
        if (pending === undefined) return;
        this.pendingRpcs.delete(msg.id);
        clearTimeout(pending.timer);
        if ('ok' in msg) {
          this.observe({
            category: 'rpc',
            event: 'call',
            outcome: 'ok',
            reason: 'resolved',
            generation,
            requestId: msg.id,
          });
          pending.resolve(msg.ok);
        } else if (typeof msg.err === 'string') {
          this.observe({
            category: 'rpc',
            event: 'call',
            outcome: 'failed',
            reason: 'server_error',
            generation,
            requestId: msg.id,
          });
          pending.reject(new Error(msg.err));
        }
      }
    }
  }

  private handleDisconnect(
    _gen: number,
    reason: string,
    errorMessage: string,
    allowReconnect: boolean,
  ): void {
    this.retire(reason, errorMessage);
    if (allowReconnect && !this.authRefused) {
      this.observe({
        category: 'connection',
        event: 'retry',
        outcome: 'ok',
        reason: 'scheduled',
        generation: this.generation,
      });
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = null;
        this.connect('reconnect');
      }, this.timeouts().reconnectMs);
    } else if (this.authRefused) {
      this.observe({
        category: 'connection',
        event: 'retry',
        outcome: 'skip',
        reason: 'auth_refused',
        generation: this.generation,
      });
    }
  }

  private clearReconnectTimer(): void {
    this.reconnectTimer = clearTimer(this.reconnectTimer);
  }

  private retire(reason: string, errorMessage: string): void {
    const oldGen = this.generation;
    if (this.ws !== null || this.reconnectTimer !== null || this.pendingRpcs.size > 0 || this.readyWaiters.length > 0) {
      this.observe({ category: 'connection', event: 'retire', outcome: 'cancelled', reason, generation: oldGen });
    }

    this.generation += 1;
    this.helloTimer = clearTimer(this.helloTimer);
    this.clearReconnectTimer();
    const oldWs = this.ws;
    this.ws = null;
    this.authenticated = false;
    if (oldWs !== null) {
      oldWs.onopen = null;
      oldWs.onmessage = null;
      oldWs.onclose = null;
      oldWs.onerror = null;
      if (oldWs.readyState === WebSocket.CONNECTING || oldWs.readyState === WebSocket.OPEN) {
        oldWs.close();
      }
    }

    if (this.ownedRecording) {
      this.wasOwnerBeforeDisconnect = true;
      this.ownedRecording = false;
    }
    this.capture.stop('retired');
    this.cancelDoneAssociation('retired');
    this.lastConnectionError = errorMessage;

    const err = new Error(errorMessage);
    const waiters = this.readyWaiters;
    this.readyWaiters = [];
    for (const w of waiters) w.reject(err);

    const pending = [...this.pendingRpcs.values()];
    this.pendingRpcs.clear();
    for (const p of pending) {
      clearTimeout(p.timer);
      this.observe({
        category: 'rpc',
        event: 'call',
        outcome: 'cancelled',
        reason: 'retired',
        generation: p.generation,
        requestId: p.id,
      });
      p.reject(err);
    }
  }

  private sendBinary(generation: number, frame: Uint8Array<ArrayBuffer>): void {
    if (generation === this.generation && this.authenticated && this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(frame);
    }
  }

  private async waitUntilAuthenticated(deadlineMs: number): Promise<number> {
    if (this.authenticated && this.ws !== null && this.ws.readyState === WebSocket.OPEN) {
      return this.generation;
    }
    if (this.authRefused) {
      throw new Error(
        this.lastConnectionError ?? 'Authentication refused by the Scriber server. Check the token and try again.',
      );
    }
    this.ensureConnected();
    await new Promise<void>((resolve, reject) => {
      const waiter: ReadyWaiter = {
        resolve: () => {
          clearTimeout(timer);
          resolve();
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      };
      const timer = setTimeout(() => {
        const idx = this.readyWaiters.indexOf(waiter);
        if (idx >= 0) this.readyWaiters.splice(idx, 1);
        reject(new Error('Request timed out waiting for the Scriber server.'));
      }, deadlineMs);
      this.readyWaiters.push(waiter);
    });
    return this.generation;
  }

  private async callWithId<T>(
    call: string,
    args: Record<string, unknown>,
  ): Promise<{ requestId: number; promise: Promise<T> }> {
    const rpcMs = this.timeouts().rpcMs;
    const gen = await this.waitUntilAuthenticated(rpcMs);
    const ws = this.ws as WebSocket;
    const id = this.allocateRequestId();

    const promise = new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pendingRpcs.delete(id);
        this.observe({
          category: 'rpc',
          event: 'call',
          outcome: 'timeout',
          reason: 'rpc_timeout',
          generation: gen,
          requestId: id,
        });
        reject(new Error('Request timed out waiting for the Scriber server.'));
      }, rpcMs);

      this.pendingRpcs.set(id, {
        generation: gen,
        id,
        timer,
        resolve: (val) => {
          resolve(val as T);
        },
        reject,
      });

      ws.send(JSON.stringify({ id, call, args }));
    });

    return { requestId: id, promise };
  }

  private allocateRequestId(): number {
    for (;;) {
      this.nextRpcId = (this.nextRpcId % 0xffff_ffff) + 1;
      if (!this.pendingRpcs.has(this.nextRpcId)) return this.nextRpcId;
    }
  }

  // ---- Storage & test-scoped diagnostics ------------------------------------

  private readStoredEndpoint(): string {
    const stored = this.readStorage(WEB_ENDPOINT_KEY);
    if (stored === null || stored.trim() === '') return window.location.origin;
    try {
      return normalizeOrigin(stored);
    } catch {
      return window.location.origin;
    }
  }

  private readStoredToken(): string {
    return this.readStorage(WEB_TOKEN_KEY) ?? '';
  }

  private readStorage(key: string): string | null {
    try {
      return window.localStorage.getItem(key);
    } catch {
      return null;
    }
  }

  private writeStorage(key: string, value: string): void {
    try {
      window.localStorage.setItem(key, value);
    } catch {
      // In-memory fallback already updated by caller.
    }
  }

  private timeouts(): WebTimeouts {
    const overrides = window.__scriberWebTest?.timeouts;
    return {
      helloMs: overrides?.helloMs ?? DEFAULT_HELLO_TIMEOUT_MS,
      reconnectMs: overrides?.reconnectMs ?? DEFAULT_RECONNECT_DELAY_MS,
      rpcMs: overrides?.rpcMs ?? DEFAULT_RPC_TIMEOUT_MS,
      drainMs: overrides?.drainMs ?? DEFAULT_DRAIN_TIMEOUT_MS,
      clipboardMs: overrides?.clipboardMs ?? DEFAULT_CLIPBOARD_TIMEOUT_MS,
    };
  }

  private observeCapture(entry: CaptureObservation): void {
    this.observe({ category: 'capture', ...entry });
  }

  private observe(entry: {
    category: WebObservation['category'];
    event: string;
    outcome: WebObservation['outcome'];
    reason: string;
    generation: number;
    requestId?: number | undefined;
    phase?: Phase | undefined;
    revision?: number | undefined;
    frames?: number | undefined;
    samples?: number | undefined;
    bytes?: number | undefined;
  }): void {
    const hook = window.__scriberWebTest;
    if (hook === undefined) return;
    const record: WebObservation = {
      seq: ++this.obsSeq,
      category: entry.category,
      event: entry.event,
      outcome: entry.outcome,
      reason: entry.reason,
      generation: entry.generation,
      ...(entry.requestId !== undefined ? { requestId: entry.requestId } : {}),
      ...(entry.phase !== undefined ? { phase: entry.phase } : {}),
      ...(entry.revision !== undefined ? { revision: entry.revision } : {}),
      ...(entry.frames !== undefined ? { frames: entry.frames } : {}),
      ...(entry.samples !== undefined ? { samples: entry.samples } : {}),
      ...(entry.bytes !== undefined ? { bytes: entry.bytes } : {}),
    };
    if (hook.observations !== undefined) {
      if (hook.observations.length >= MAX_OBSERVATIONS) hook.observations.shift();
      hook.observations.push(record);
    }
  }
}
