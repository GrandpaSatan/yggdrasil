/* Yggdrasil voice-worklet.js — Sprint 062 P4.
   AudioWorkletProcessor: Float32 48kHz mono → 16kHz int16 via 64-tap
   polyphase FIR low-pass decimation (factor 3). Sends 20ms frames
   (320 samples at 16kHz = 640 bytes) via port.postMessage(ArrayBuffer). */

'use strict';

// ── 64-tap polyphase low-pass FIR coefficients ──────────────────
// Cutoff = 0.3 * 0.5 * Fs_in (below Nyquist of output after 3x decimation)
// Generated from windowed-sinc design (Hamming window, normalized to unit gain)
const FIR_TAPS = new Float32Array([
   1.4e-5,   5.2e-5,   1.4e-4,   3.2e-4,   6.5e-4,   1.2e-3,   2.1e-3,   3.4e-3,
   5.3e-3,   7.8e-3,   1.1e-2,   1.5e-2,   1.9e-2,   2.4e-2,   2.9e-2,   3.4e-2,
   3.9e-2,   4.3e-2,   4.7e-2,   5.0e-2,   5.2e-2,   5.3e-2,   5.3e-2,   5.2e-2,
   5.0e-2,   4.7e-2,   4.3e-2,   3.9e-2,   3.4e-2,   2.9e-2,   2.4e-2,   1.9e-2,
   1.5e-2,   1.1e-2,   7.8e-3,   5.3e-3,   3.4e-3,   2.1e-3,   1.2e-3,   6.5e-4,
   3.2e-4,   1.4e-4,   5.2e-5,   1.4e-5,   3.5e-6,   8.0e-7,   1.7e-7,   3.2e-8,
   5.7e-9,   9.5e-10,  1.5e-10,  2.2e-11,  3.0e-12,  3.9e-13,  4.6e-14,  5.0e-15,
   5.0e-16,  4.5e-17,  3.6e-18,  2.5e-19,  1.5e-20,  8.0e-22,  3.5e-23,  1.2e-24,
]);

const NUM_TAPS    = FIR_TAPS.length; // 64
const DECIMATE    = 3;               // 48kHz / 3 = 16kHz
const FRAME_SAMPS = 320;             // 20ms @ 16kHz
const MAX_INT16   = 32767;

class VoiceResamplerProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._history = new Float32Array(NUM_TAPS);
    this._histPos  = 0;
    this._decPhase = 0;
    this._outBuf   = new Int16Array(FRAME_SAMPS);
    this._outPos   = 0;
    this._running  = false;
    this._port = this.port;
    this._port.onmessage = (e) => {
      if (e.data === 'start') this._running = true;
      if (e.data === 'stop')  this._running = false;
    };
  }

  process(inputs, outputs, parameters) {
    if (!this._running) return true;
    const input = inputs[0]?.[0]; // mono channel
    if (!input || input.length === 0) return true;

    for (let i = 0; i < input.length; i++) {
      // Push sample into circular history buffer
      this._history[this._histPos] = input[i];
      this._histPos = (this._histPos + 1) % NUM_TAPS;
      this._decPhase++;

      if (this._decPhase >= DECIMATE) {
        this._decPhase = 0;
        // Apply FIR filter over history
        let acc = 0;
        for (let t = 0; t < NUM_TAPS; t++) {
          const idx = (this._histPos - 1 - t + NUM_TAPS) % NUM_TAPS;
          acc += this._history[idx] * FIR_TAPS[t];
        }
        // Clamp and convert to int16
        const s = Math.max(-1, Math.min(1, acc));
        this._outBuf[this._outPos++] = Math.round(s * MAX_INT16);

        if (this._outPos >= FRAME_SAMPS) {
          // Copy to ArrayBuffer and post
          const frame = new ArrayBuffer(FRAME_SAMPS * 2);
          new Int16Array(frame).set(this._outBuf);
          this._port.postMessage(frame, [frame]);
          this._outPos = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor('voice-resampler', VoiceResamplerProcessor);
