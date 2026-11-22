import { expect, test } from '@playwright/test';

import {
  CEILING_DBFS,
  encodePcm16Le,
  floatToPcm16,
  FLOOR_DBFS,
  FRAME_BYTES,
  FRAME_SAMPLES,
  LevelMeter,
  Pcm16Framer,
  TARGET_SAMPLE_RATE,
  WINDOW_MS,
} from '../../src/lib/pcm16';

function decodeLeInt16(bytes: Uint8Array): Int16Array {
  expect(bytes.byteLength % 2).toBe(0);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out = new Int16Array(bytes.byteLength / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = view.getInt16(i * 2, true);
  }
  return out;
}

function concatBytes(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.byteLength;
  }
  return out;
}

function runStream(input: Float32Array, rate: number, blockSizes: readonly number[]): Uint8Array[] {
  const framer = new Pcm16Framer(rate);
  const frames: Uint8Array[] = [];
  let offset = 0;
  let sizeIdx = 0;
  while (offset < input.length) {
    const size = blockSizes[sizeIdx % blockSizes.length] ?? 128;
    sizeIdx += 1;
    const end = Math.min(input.length, offset + size);
    frames.push(...framer.push(input.subarray(offset, end)));
    offset = end;
  }
  const tail = framer.flush();
  if (tail !== null) frames.push(tail);
  return frames;
}

test.describe('pcm16 resampling and framing', () => {
  test('clips out-of-range and non-finite floats into signed little-endian PCM16', () => {
    expect(floatToPcm16(0)).toBe(0);
    expect(floatToPcm16(1)).toBe(32_767);
    expect(floatToPcm16(2.5)).toBe(32_767);
    expect(floatToPcm16(-1)).toBe(-32_768);
    expect(floatToPcm16(-99)).toBe(-32_768);
    expect(floatToPcm16(0.5)).toBe(16_384);
    expect(floatToPcm16(-0.5)).toBe(-16_384);
    expect(floatToPcm16(Number.NaN)).toBe(0);
    expect(floatToPcm16(Number.POSITIVE_INFINITY)).toBe(0);
    expect(floatToPcm16(Number.NEGATIVE_INFINITY)).toBe(0);

    // Little-endian byte order check: 0x0102 -> [0x02, 0x01], -32768 (0x8000) -> [0x00, 0x80].
    const encoded = encodePcm16Le([0x0102, -32_768, 32_767, -1]);
    expect([...encoded]).toEqual([0x02, 0x01, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff]);
    expect([...decodeLeInt16(encoded)]).toEqual([0x0102, -32_768, 32_767, -1]);
  });

  test('validates constructor parameters and handles empty pushes/flushes', () => {
    expect(() => new Pcm16Framer(0)).toThrow(RangeError);
    expect(() => new Pcm16Framer(-48_000)).toThrow(RangeError);
    expect(() => new Pcm16Framer(Number.NaN)).toThrow(RangeError);
    expect(() => new Pcm16Framer(48_000, 0)).toThrow(RangeError);
    expect(() => new Pcm16Framer(48_000, 1.5)).toThrow(RangeError);
    expect(() => new Pcm16Framer(48_000, FRAME_SAMPLES, 0)).toThrow(RangeError);

    const framer = new Pcm16Framer(TARGET_SAMPLE_RATE);
    expect(framer.push(new Float32Array(0))).toEqual([]);
    expect(framer.flush()).toBeNull();
  });

  test('resamples 48 kHz mono to 16 kHz in 4,000-sample frames with a nonempty short tail', () => {
    // 1.1 seconds at 48 kHz = 52,800 samples -> 17,600 samples at 16 kHz = 4 full frames (16,000) + 1,600-sample tail.
    const rate = 48_000;
    const totalInput = 52_800;
    const input = new Float32Array(totalInput);
    for (let i = 0; i < totalInput; i++) {
      input[i] = Math.sin((2 * Math.PI * 440 * i) / rate) * 0.75;
    }

    const frames = runStream(input, rate, [128]);
    expect(frames).toHaveLength(5);
    for (let i = 0; i < 4; i++) {
      expect(frames[i]?.byteLength).toBe(FRAME_BYTES);
    }
    expect(frames[4]?.byteLength).toBe(1_600 * 2);

    const decoded = decodeLeInt16(concatBytes(frames));
    expect(decoded).toHaveLength(17_600);
    // Exact 3:1 decimation points match input[3 * n].
    for (const idx of [0, 1, 17, 4_000, 12_345, 17_599]) {
      expect(decoded[idx]).toBe(floatToPcm16(input[idx * 3] ?? 0));
    }
  });

  test('resamples 44.1 kHz mono to 16 kHz with exact frame sizing and nonempty short tail', () => {
    // 1.01 seconds at 44.1 kHz = 44,541 samples -> 16,160 samples at 16 kHz = 4 full frames + 160-sample tail.
    const rate = 44_100;
    const totalInput = 44_100 + 441;
    const input = new Float32Array(totalInput);
    for (let i = 0; i < totalInput; i++) {
      input[i] = Math.sin((2 * Math.PI * 300 * i) / rate) * 0.6;
    }

    const frames = runStream(input, rate, [128]);
    expect(frames).toHaveLength(5);
    for (let i = 0; i < 4; i++) {
      expect(frames[i]?.byteLength).toBe(FRAME_BYTES);
    }
    expect(frames[4]?.byteLength).toBe(160 * 2);

    const decoded = decodeLeInt16(concatBytes(frames));
    expect(decoded).toHaveLength(16_160);
    // Spot-check linear interpolation at fractional sample positions.
    for (const n of [0, 1, 13, 3_999, 8_000, 16_159]) {
      const pos = (n * rate) / TARGET_SAMPLE_RATE;
      const i0 = Math.floor(pos);
      const frac = pos - i0;
      const s0 = input[i0] ?? 0;
      const s1 = input[i0 + 1] ?? s0;
      expect(decoded[n]).toBe(floatToPcm16(s0 + (s1 - s0) * frac));
    }
  });

  test('is strictly block-boundary invariant across arbitrary chunking at 44.1 kHz and 48 kHz', () => {
    for (const rate of [44_100, 48_000, 16_000]) {
      const len = Math.floor(rate * 0.63) + 7;
      const signal = new Float32Array(len);
      for (let i = 0; i < len; i++) {
        // Deliberately include excursions past [-1, 1] to verify clipping invariance too.
        signal[i] = Math.sin((2 * Math.PI * 523.25 * i) / rate) * 1.25;
      }

      const whole = concatBytes(runStream(signal, rate, [len]));
      const by128 = concatBytes(runStream(signal, rate, [128]));
      const ragged = concatBytes(runStream(signal, rate, [1, 7, 13, 64, 257, 3, 512, 19]));
      const byOne = concatBytes(runStream(signal, rate, [1]));

      expect(by128).toEqual(whole);
      expect(ragged).toEqual(whole);
      expect(byOne).toEqual(whole);
    }
  });

  test('handles exact-multiple input with no empty tail and flushes trailing fractional sample cleanly', () => {
    // Exact 16 kHz full frame -> flush returns null (no empty tail).
    const exact = new Float32Array(FRAME_SAMPLES);
    exact.fill(0.25);
    const f1 = new Pcm16Framer(TARGET_SAMPLE_RATE);
    const emitted = f1.push(exact);
    expect(emitted).toHaveLength(1);
    expect(emitted[0]?.byteLength).toBe(FRAME_BYTES);
    expect(f1.flush()).toBeNull();

    // Very short 44.1 kHz input whose trailing fractional index lands on the last sample.
    const f2 = new Pcm16Framer(44_100, 4);
    expect(f2.push(new Float32Array([0.1, 0.2, 0.3]))).toEqual([]);
    const tail = f2.flush();
    expect(tail).not.toBeNull();
    expect(decodeLeInt16(tail ?? new Uint8Array(0))).toEqual(
      new Int16Array([floatToPcm16(0.1), floatToPcm16(0.3)]),
    );

    // Upsampling path exercises multi-frame flush merge when flush emits more than one frame.
    const f3 = new Pcm16Framer(8_000, 2, 16_000);
    f3.push(new Float32Array([0.5]));
    const upsampled = f3.flush();
    expect(upsampled).not.toBeNull();
    expect(decodeLeInt16(upsampled ?? new Uint8Array(0))).toEqual(
      new Int16Array([floatToPcm16(0.5), floatToPcm16(0.5)]),
    );
  });
});

function runMeter(input: Float32Array, rate: number, blockSizes: readonly number[]): number[] {
  const meter = new LevelMeter(rate);
  const readings: number[] = [];
  let offset = 0;
  let sizeIdx = 0;
  while (offset < input.length) {
    const size = blockSizes[sizeIdx % blockSizes.length] ?? 128;
    sizeIdx += 1;
    const end = Math.min(input.length, offset + size);
    readings.push(...meter.push(input.subarray(offset, end)));
    offset = end;
  }
  return readings;
}

test.describe('LevelMeter 50 ms RMS dBFS metering', () => {
  test('pins normalized RMS dBFS scale for silence, NaN, below-floor, -29 dBFS midpoint and above-ceiling', () => {
    expect(WINDOW_MS).toBe(50);
    expect(FLOOR_DBFS).toBe(-52.0);
    expect(CEILING_DBFS).toBe(-6.0);

    const meter = new LevelMeter(48_000);
    expect(meter.windowSamples).toBe(2_400);
    expect(meter.push(new Float32Array(0))).toEqual([]);

    // Exact 0 for silence.
    expect(meter.push(new Float32Array(2_400))).toEqual([0]);

    // Exact 0 for NaN window.
    const nanWin = new Float32Array(2_400).fill(0.5);
    nanWin[1_234] = Number.NaN;
    expect(meter.push(nanWin)).toEqual([0]);

    // Exact 0 for a below-floor window (-60 dBFS < -52 dBFS) and at-floor (-52 dBFS).
    expect(meter.push(new Float32Array(2_400).fill(10 ** (-60 / 20)))).toEqual([0]);
    expect(meter.push(new Float32Array(2_400).fill(10 ** (FLOOR_DBFS / 20)))).toEqual([0]);

    // Exact 1 above the ceiling (-3 dBFS and 0 dBFS full-scale).
    expect(meter.push(new Float32Array(2_400).fill(10 ** (-3 / 20)))).toEqual([1]);
    expect(meter.push(new Float32Array(2_400).fill(1.0))).toEqual([1]);

    // Midpoint at -29 dBFS -> 0.5 within 6 decimal places.
    const mid = meter.push(new Float32Array(2_400).fill(10 ** (-29 / 20)));
    expect(mid).toHaveLength(1);
    expect(mid[0]).toBeCloseTo(0.5, 6);
  });

  test('emits 20 ordered readings per second of 44.1 and 48 kHz samples across 128-sample and multi-window blocks', () => {
    for (const rate of [44_100, 48_000]) {
      const windowSamples = Math.floor((rate * WINDOW_MS) / 1_000);
      const signal = new Float32Array(rate);
      const expected: number[] = [];
      for (let w = 0; w < 20; w++) {
        const dbfs = -50 + w * 2; // -50 dBFS .. -12 dBFS, strictly increasing across all 20 windows
        const amp = 10 ** (dbfs / 20);
        signal.fill(amp, w * windowSamples, (w + 1) * windowSamples);
        expected.push((dbfs - FLOOR_DBFS) / (CEILING_DBFS - FLOOR_DBFS));
      }

      const by128 = runMeter(signal, rate, [128]);
      const multiWindow = runMeter(signal, rate, [windowSamples * 3 + 17, windowSamples * 2 - 5, windowSamples * 5]);
      const whole = runMeter(signal, rate, [rate]);

      expect(by128).toHaveLength(20);
      expect(multiWindow).toHaveLength(20);
      expect(whole).toHaveLength(20);

      for (let w = 0; w < 20; w++) {
        expect(by128[w]).toBeCloseTo(expected[w] ?? 0, 6);
        expect(multiWindow[w]).toBeCloseTo(expected[w] ?? 0, 6);
        expect(whole[w]).toBeCloseTo(expected[w] ?? 0, 6);
        if (w > 0) {
          expect(by128[w] ?? 0).toBeGreaterThan(by128[w - 1] ?? 0);
        }
      }
    }
  });

  test('yields no readings for NaN, positive infinity, -1, 0 and 19 Hz', () => {
    const samples = new Float32Array(10_000).fill(0.5);
    for (const rate of [Number.NaN, Number.POSITIVE_INFINITY, -1, 0, 19]) {
      const meter = new LevelMeter(rate);
      expect(meter.windowSamples).toBe(0);
      expect(meter.push(samples)).toEqual([]);
    }
  });

  test('recovers on the first finite window following a NaN window, including across partial blocks', () => {
    const meter = new LevelMeter(48_000);
    const midAmp = 10 ** (-29 / 20);

    // Partial NaN block carried over into the rest of the first window -> emits exact 0 and resets.
    const firstHalf = new Float32Array(1_200).fill(midAmp);
    firstHalf[10] = Number.NaN;
    expect(meter.push(firstHalf)).toEqual([]);

    const secondHalfAndNext = new Float32Array(1_200 + 2_400).fill(midAmp);
    const readings = meter.push(secondHalfAndNext);
    expect(readings).toHaveLength(2);
    expect(readings[0]).toBe(0);
    expect(readings[1]).toBeCloseTo(0.5, 6);
  });
});
