// Pure streaming audio resampling and PCM16 little-endian framing for the web
// backend. Zero browser or DOM dependencies so unit tests exercise it directly
// under Node.

/** Target sample rate expected by `scriber-server`'s `StreamRecorder` (16 kHz mono). */
export const TARGET_SAMPLE_RATE = 16_000;

/** Samples per full 250 ms binary frame at 16 kHz. */
export const FRAME_SAMPLES = 4_000;

/** Byte length of a full 250 ms PCM16 frame (4,000 samples * 2 bytes). */
export const FRAME_BYTES = FRAME_SAMPLES * 2;

/** Clips one floating-point sample to [-1, 1] and quantizes to signed 16-bit PCM. */
export function floatToPcm16(sample: number): number {
  if (!Number.isFinite(sample)) return 0;
  const clamped = Math.max(-1, Math.min(1, sample));
  const scaled = clamped < 0 ? Math.round(clamped * 32_768) : Math.round(clamped * 32_767);
  return scaled === 0 ? 0 : scaled;
}

/** Encodes signed 16-bit samples as little-endian PCM16 bytes. */
export function encodePcm16Le(samples: Int16Array | readonly number[]): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(samples.length * 2);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let i = 0; i < samples.length; i++) {
    view.setInt16(i * 2, samples[i] ?? 0, true);
  }
  return bytes;
}

/**
 * Streaming linear resampler and fixed-size PCM16 framer.
 *
 * Output sample positions are computed directly from the monotonic output index
 * `(outputIndex * inputSampleRate) / targetSampleRate` rather than accumulated
 * floating-point additions, so output is strictly invariant across arbitrary
 * input block boundaries and does not drift on non-binary ratios like 44.1 kHz.
 */
export class Pcm16Framer {
  private readonly inputSampleRate: number;
  private readonly targetSampleRate: number;
  private readonly frameSamples: number;
  private pendingInput: Float32Array = new Float32Array(0);
  private inputBaseIndex = 0;
  private outputIndex = 0;
  private readonly frameBuf: Int16Array;
  private frameFill = 0;

  constructor(inputSampleRate: number, frameSamples = FRAME_SAMPLES, targetSampleRate = TARGET_SAMPLE_RATE) {
    if (!Number.isFinite(inputSampleRate) || inputSampleRate <= 0) throw new RangeError('Input sample rate must be positive.');
    if (!Number.isInteger(frameSamples) || frameSamples <= 0) throw new RangeError('Frame size must be a positive integer.');
    if (!Number.isFinite(targetSampleRate) || targetSampleRate <= 0) throw new RangeError('Target sample rate must be positive.');
    this.inputSampleRate = inputSampleRate;
    this.frameSamples = frameSamples;
    this.targetSampleRate = targetSampleRate;
    this.frameBuf = new Int16Array(frameSamples);
  }

  /** Pushes one mono Float32 block and returns every completed full PCM16 frame. */
  push(block: Float32Array): Uint8Array<ArrayBuffer>[] {
    if (block.length === 0) return [];
    const combined = new Float32Array(this.pendingInput.length + block.length);
    combined.set(this.pendingInput, 0);
    combined.set(block, this.pendingInput.length);
    this.pendingInput = combined;

    const frames: Uint8Array<ArrayBuffer>[] = [];
    const len = this.pendingInput.length;
    for (;;) {
      const localPos = (this.outputIndex * this.inputSampleRate) / this.targetSampleRate - this.inputBaseIndex;
      const idx = Math.floor(localPos);
      const frac = localPos - idx;
      if (frac === 0 ? idx >= len : idx + 1 >= len) {
        const drop = Math.min(len, Math.max(0, idx));
        if (drop > 0) {
          this.pendingInput = this.pendingInput.slice(drop);
          this.inputBaseIndex += drop;
        }
        break;
      }
      const s0 = this.pendingInput[idx] ?? 0;
      const s1 = frac === 0 ? s0 : (this.pendingInput[idx + 1] ?? s0);
      this.frameBuf[this.frameFill] = floatToPcm16(frac === 0 ? s0 : s0 + (s1 - s0) * frac);
      this.frameFill += 1;
      this.outputIndex += 1;
      if (this.frameFill === this.frameSamples) {
        frames.push(encodePcm16Le(this.frameBuf));
        this.frameFill = 0;
      }
    }
    return frames;
  }

  /**
   * Flushes any trailing fractional input sample and buffered output samples as
   * one nonempty PCM16 tail frame (or null when nothing remains), resetting state.
   */
  flush(): Uint8Array<ArrayBuffer> | null {
    const len = this.pendingInput.length;
    const out: number[] = [];
    for (let i = 0; i < this.frameFill; i++) out.push(this.frameBuf[i] ?? 0);
    for (;;) {
      const idx = Math.floor((this.outputIndex * this.inputSampleRate) / this.targetSampleRate - this.inputBaseIndex);
      if (idx >= len) break;
      out.push(floatToPcm16(this.pendingInput[idx] ?? 0));
      this.outputIndex += 1;
    }
    this.pendingInput = new Float32Array(0);
    this.inputBaseIndex = 0;
    this.outputIndex = 0;
    this.frameFill = 0;
    return out.length === 0 ? null : encodePcm16Le(out);
  }
}
