// Browser microphone capture for the web backend. Starts getUserMedia and
// AudioContext activation synchronously inside the user gesture, prepares the
// static AudioWorklet before dispatching Record/Resume, emits 16 kHz PCM16
// frames only after local action success, and drains in order on Pause/Done.

import { LevelMeter, Pcm16Framer } from './pcm16';
import type { Phase } from './types';

declare global {
  class AudioWorkletProcessor {
    readonly port: MessagePort;
    constructor();
    process(
      inputs: Float32Array[][],
      outputs: Float32Array[][],
      parameters: Record<string, Float32Array>,
    ): boolean;
  }
  function registerProcessor(name: string, processorCtor: new () => AudioWorkletProcessor): void;
}

/** Static path served alongside the built web app (no blob: or inline worklet). */
export const WORKLET_URL = '/capture-worklet.js';

/** Default deadline for the worklet to acknowledge an ordered drain. */
export const DEFAULT_DRAIN_TIMEOUT_MS = 1_000;

/** Microphone constraints specified in docs/design-web.md. */
export const CAPTURE_CONSTRAINTS: MediaStreamConstraints = {
  audio: {
    channelCount: 1,
    echoCancellation: true,
    noiseSuppression: true,
  },
};

export interface CaptureObservation {
  event: 'start' | 'drain' | 'stop' | 'meter';
  outcome: 'ok' | 'skip' | 'timeout' | 'cancelled' | 'failed';
  reason: string;
  generation: number;
  source?: 'local';
  armed?: boolean;
  draining?: boolean;
  sampleRate?: number;
  windowSamples?: number;
  completedWindows?: number;
  frames?: number;
  samples?: number;
  bytes?: number;
}

export interface CaptureDrainResult {
  frames: number;
  samples: number;
  bytes: number;
  tailSamples: number;
}

export interface CaptureCallbacks {
  sendFrame: (frame: Uint8Array<ArrayBuffer>) => void;
  onLevel: (level: number) => void;
  observe: (entry: CaptureObservation) => void;
}

export interface CaptureHandle {
  arm: () => boolean;
  cancel: (reason: string) => void;
}

type WorkletMessage =
  | { type: 'audio'; samples: Float32Array }
  | { type: 'drained'; id: number };

interface CaptureSession {
  generation: number;
  expectedPrePhase: 'ready' | 'paused';
  callbacks: CaptureCallbacks;
  stream: MediaStream | null;
  context: AudioContext;
  source: MediaStreamAudioSourceNode | null;
  worklet: AudioWorkletNode | null;
  zeroGain: GainNode | null;
  framer: Pcm16Framer | null;
  meter: LevelMeter | null;
  dispatching: boolean;
  armed: boolean;
  draining: boolean;
  closed: boolean;
  cancelReason: string | null;
  drainId: number;
  drainResolve: (() => void) | null;
  drainReject: ((err: Error) => void) | null;
  drainTimer: ReturnType<typeof setTimeout> | null;
  framesSent: number;
  samplesSent: number;
  bytesSent: number;
  completedWindows: number;
}

function clearTimer(timer: ReturnType<typeof setTimeout> | null): null {
  if (timer !== null) clearTimeout(timer);
  return null;
}

function readableError(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function isPermissionDenied(err: unknown): boolean {
  return err instanceof Error && err.name === 'NotAllowedError';
}

function stopTracks(stream: MediaStream | null): void {
  if (stream === null) return;
  for (const track of stream.getTracks()) track.stop();
}

export class BrowserCapture {
  private active: CaptureSession | null = null;

  private isAlive(session: CaptureSession): boolean {
    return !session.closed && this.active === session;
  }

  /** True while a local capture is armed and streaming audio. */
  isActive(): boolean {
    return this.active !== null && this.active.armed && !this.active.closed;
  }

  /** True while a local capture is draining before Pause or Done. */
  isDraining(): boolean {
    return this.active !== null && this.active.draining && !this.active.closed;
  }

  /**
   * Starts microphone capture synchronously inside a click or keydown gesture.
   * Calls getUserMedia and AudioContext activation before the first await, then
   * finishes worklet setup before returning an arm/cancel handle for RPC dispatch.
   */
  async startGesture(
    generation: number,
    expectedPrePhase: 'ready' | 'paused',
    callbacks: CaptureCallbacks,
  ): Promise<CaptureHandle> {
    this.stop('superseded');

    const mediaDevices = navigator.mediaDevices as MediaDevices | undefined;
    if (mediaDevices === undefined) {
      callbacks.observe({ event: 'start', outcome: 'failed', reason: 'setup_error', generation });
      throw new Error('Could not access the microphone: browser media capture is unavailable.');
    }

    let context: AudioContext;
    let streamPromise: Promise<MediaStream>;
    try {
      streamPromise = mediaDevices.getUserMedia(CAPTURE_CONSTRAINTS);
      context = new AudioContext();
      if (context.state === 'suspended') void context.resume();
    } catch (err) {
      callbacks.observe({ event: 'start', outcome: 'failed', reason: 'setup_error', generation });
      throw new Error(`Could not set up the microphone: ${readableError(err)}`);
    }

    const session: CaptureSession = {
      generation,
      expectedPrePhase,
      callbacks,
      stream: null,
      context,
      source: null,
      worklet: null,
      zeroGain: null,
      framer: null,
      meter: null,
      dispatching: false,
      armed: false,
      draining: false,
      closed: false,
      cancelReason: null,
      drainId: 0,
      drainResolve: null,
      drainReject: null,
      drainTimer: null,
      framesSent: 0,
      samplesSent: 0,
      bytesSent: 0,
      completedWindows: 0,
    };
    this.active = session;

    // Ensure a late-resolving stream is always stopped if the session was torn down first.
    streamPromise.then(
      (lateStream) => {
        if (session.closed) stopTracks(lateStream);
      },
      () => undefined,
    );

    let stream: MediaStream;
    try {
      stream = await streamPromise;
    } catch (err) {
      const reason = isPermissionDenied(err) ? 'permission_denied' : 'setup_error';
      const alreadyCancelled = session.closed;
      this.teardown(session, reason);
      if (!alreadyCancelled) callbacks.observe({ event: 'start', outcome: 'failed', reason, generation });
      throw new Error(`Could not access the microphone: ${readableError(err)}`);
    }

    if (!this.isAlive(session)) {
      throw new Error('Recording was cancelled before microphone setup finished.');
    }
    session.stream = stream;

    try {
      await session.context.audioWorklet.addModule(WORKLET_URL);
      const source = session.context.createMediaStreamSource(stream);
      const worklet = new AudioWorkletNode(session.context, 'scriber-capture-processor');
      const zeroGain = session.context.createGain();
      zeroGain.gain.value = 0;

      session.source = source;
      session.worklet = worklet;
      session.zeroGain = zeroGain;
      session.framer = new Pcm16Framer(session.context.sampleRate);
      session.meter = new LevelMeter(session.context.sampleRate);
      callbacks.observe({
        event: 'meter',
        outcome: 'ok',
        reason: 'setup',
        generation,
        source: 'local',
        armed: session.armed,
        draining: session.draining,
        sampleRate: session.meter.sampleRate,
        windowSamples: session.meter.windowSamples,
        completedWindows: 0,
      });

      worklet.port.onmessage = (event: MessageEvent<WorkletMessage>) => {
        this.onWorkletMessage(session, event.data);
      };

      source.connect(worklet);
      worklet.connect(zeroGain);
      zeroGain.connect(session.context.destination);
      if (session.context.state !== 'running') {
        await session.context.resume();
      }
    } catch (err) {
      this.teardown(session, 'setup_error');
      callbacks.observe({ event: 'start', outcome: 'failed', reason: 'setup_error', generation });
      throw new Error(`Could not set up the microphone: ${readableError(err)}`);
    }

    session.dispatching = true;
    return {
      arm: () => {
        if (!this.isAlive(session)) return false;
        session.armed = true;
        callbacks.observe({ event: 'start', outcome: 'ok', reason: 'armed', generation });
        return true;
      },
      cancel: (reason: string) => {
        callbacks.observe({ event: 'start', outcome: 'cancelled', reason, generation });
        this.teardown(session, reason);
      },
    };
  }

  /** Handles remote session phase changes during pending setup or active recording. */
  handleRemotePhase(phase: Phase): void {
    const session = this.active;
    if (session === null || session.closed) return;
    if (!session.armed && !session.draining) {
      if (phase !== session.expectedPrePhase && !(session.dispatching && phase === 'recording')) {
        session.callbacks.observe({ event: 'start', outcome: 'cancelled', reason: 'remote_interruption', generation: session.generation });
        this.teardown(session, 'remote_interruption');
      }
      return;
    }
    if (session.armed && !session.draining && phase !== 'recording') {
      session.callbacks.observe({
        event: 'stop',
        outcome: 'cancelled',
        reason: 'remote_departure',
        generation: session.generation,
        frames: session.framesSent,
        samples: session.samplesSent,
        bytes: session.bytesSent,
      });
      this.teardown(session, 'remote_departure');
    }
  }

  /**
   * Flushes all buffered audio in order before Pause or Done. Waits for the
   * worklet's ordered drain acknowledgement (bounded by timeoutMs), sends any
   * nonempty PCM16 tail, and tears down tracks and audio nodes before resolving.
   */
  async drainAndStop(generation: number, timeoutMs = DEFAULT_DRAIN_TIMEOUT_MS): Promise<CaptureDrainResult | null> {
    const session = this.active;
    if (session === null || session.worklet === null || session.framer === null || session.generation !== generation) return null;

    session.draining = true;
    const drainId = ++session.drainId;
    const worklet = session.worklet;

    try {
      await new Promise<void>((resolve, reject) => {
        session.drainResolve = resolve;
        session.drainReject = reject;
        session.drainTimer = setTimeout(() => {
          session.drainTimer = null;
          session.drainResolve = null;
          session.drainReject = null;
          const { framesSent: frames, samplesSent: samples, bytesSent: bytes } = session;
          this.teardown(session, 'drain_timeout');
          session.callbacks.observe({ event: 'drain', outcome: 'timeout', reason: 'drain_timeout', generation: session.generation, frames, samples, bytes });
          reject(new Error('Timed out waiting for microphone audio to finish flushing.'));
        }, timeoutMs);
        worklet.port.postMessage({ type: 'drain', id: drainId });
      });
    } catch (err) {
      this.teardown(session, 'drain_failed');
      throw err;
    }

    let tailSamples = 0;
    const tail = session.framer.flush();
    if (tail !== null && tail.byteLength > 0) {
      tailSamples = tail.byteLength / 2;
      session.framesSent += 1;
      session.samplesSent += tailSamples;
      session.bytesSent += tail.byteLength;
      session.callbacks.sendFrame(tail);
    }

    const result: CaptureDrainResult = {
      frames: session.framesSent,
      samples: session.samplesSent,
      bytes: session.bytesSent,
      tailSamples,
    };
    this.teardown(session, 'drained');
    session.callbacks.observe({ event: 'drain', outcome: 'ok', reason: 'drained', generation: session.generation, frames: result.frames, samples: result.samples, bytes: result.bytes });
    return result;
  }

  /** Immediately stops and cleans up any pending, active or draining capture. */
  stop(reason: string): void {
    const session = this.active;
    if (session === null) return;
    if (!session.closed) {
      session.callbacks.observe({
        event: session.draining ? 'drain' : 'stop',
        outcome: reason === 'discard' ? 'ok' : 'cancelled',
        reason,
        generation: session.generation,
        frames: session.framesSent,
        samples: session.samplesSent,
        bytes: session.bytesSent,
      });
    }
    this.teardown(session, reason);
  }

  private onWorkletMessage(session: CaptureSession, msg: WorkletMessage): void {
    if (!this.isAlive(session)) return;
    if (msg.type === 'audio') {
      if (!session.armed || session.framer === null) return;
      for (const frame of session.framer.push(msg.samples)) {
        session.framesSent += 1;
        session.samplesSent += frame.byteLength / 2;
        session.bytesSent += frame.byteLength;
        session.callbacks.sendFrame(frame);
      }
      if (!session.draining && session.meter !== null) {
        const readings = session.meter.push(msg.samples);
        session.completedWindows += readings.length;
        for (const level of readings) {
          session.callbacks.onLevel(level);
        }
      }
      return;
    }
    if (msg.id === session.drainId && session.drainResolve !== null) {
      const resolve = session.drainResolve;
      session.drainResolve = null;
      session.drainReject = null;
      session.drainTimer = clearTimer(session.drainTimer);
      resolve();
    }
  }

  private teardown(session: CaptureSession, reason: string): void {
    if (session.closed) return;
    const wasArmed = session.armed;
    const wasDraining = session.draining;
    const meter = session.meter;
    session.closed = true;
    session.armed = false;
    session.draining = false;
    session.meter = null;
    session.cancelReason = reason;
    if (this.active === session) this.active = null;
    if (meter !== null) {
      const outcome: CaptureObservation['outcome'] =
        reason === 'drained' || reason === 'discard' ? 'ok' : reason === 'drain_timeout' ? 'timeout' : 'cancelled';
      session.callbacks.observe({
        event: 'meter',
        outcome,
        reason,
        generation: session.generation,
        source: 'local',
        armed: wasArmed,
        draining: wasDraining,
        sampleRate: meter.sampleRate,
        windowSamples: meter.windowSamples,
        completedWindows: session.completedWindows,
      });
    }

    session.drainTimer = clearTimer(session.drainTimer);
    if (session.drainReject !== null) {
      const reject = session.drainReject;
      session.drainResolve = null;
      session.drainReject = null;
      reject(new Error(`Microphone capture stopped: ${reason}.`));
    }
    if (session.worklet !== null) {
      session.worklet.port.onmessage = null;
      session.worklet.port.postMessage({ type: 'stop' });
      session.worklet.disconnect();
      session.worklet = null;
    }
    if (session.source !== null) {
      session.source.disconnect();
      session.source = null;
    }
    if (session.zeroGain !== null) {
      session.zeroGain.disconnect();
      session.zeroGain = null;
    }
    stopTracks(session.stream);
    session.stream = null;
    if (session.context.state !== 'closed') void session.context.close();
  }
}
