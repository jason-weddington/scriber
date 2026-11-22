// Static AudioWorklet processor for Scriber's web build. Loaded by URL from
// '/capture-worklet.js' so the page CSP never needs blob: or 'unsafe-inline'.
// Downmixes all input channels to mono and acknowledges drain requests in order.

/** @typedef {{ type: 'drain'; id: number } | { type: 'stop' }} WorkletControlMessage */

class ScriberCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.stopped = false;
    /** @param {MessageEvent<WorkletControlMessage>} event */
    this.port.onmessage = (event) => {
      const msg = event.data;
      if (msg.type === 'drain') {
        this.stopped = true;
        this.port.postMessage({ type: 'drained', id: msg.id });
      } else {
        this.stopped = true;
      }
    };
  }

  /**
   * @override
   * @param {Float32Array[][]} inputs
   * @returns {boolean}
   */
  process(inputs) {
    if (this.stopped) return false;
    const input = inputs[0];
    if (input === undefined || input.length === 0) return true;
    const first = input[0];
    if (first === undefined || first.length === 0) return true;

    const frameCount = first.length;
    const mono = new Float32Array(frameCount);
    if (input.length === 1) {
      mono.set(first);
    } else {
      const channelCount = input.length;
      for (let i = 0; i < frameCount; i++) {
        let sum = 0;
        for (let ch = 0; ch < channelCount; ch++) {
          const channel = input[ch];
          sum += channel !== undefined ? (channel[i] ?? 0) : 0;
        }
        mono[i] = sum / channelCount;
      }
    }
    this.port.postMessage({ type: 'audio', samples: mono }, [mono.buffer]);
    return true;
  }
}

registerProcessor('scriber-capture-processor', ScriberCaptureProcessor);
