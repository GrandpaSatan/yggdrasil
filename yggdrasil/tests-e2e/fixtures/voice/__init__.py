"""Synthetic voice fixtures.

The voice server's VAD calibrates against the first ~2 seconds of audio and
rejects anything below 0.5× the adaptive noise floor. A pure-silence fixture
would therefore be dropped before it reaches the STT pipeline — the test would
hang waiting for a ``transcript`` frame that never arrives.

``synthetic_speech_wav()`` builds a deterministic 3.8-second s16le PCM WAV at
16 kHz containing:

* 2.0 s of silence (VAD calibration window — noise floor ≈ 0).
* 0.8 s of a 440 Hz sine at 0.15 amplitude (≈ -16 dBFS, well above the 3.0×
  onset threshold with a near-zero noise floor).
* 1.0 s of trailing silence (triggers VAD endpoint after the 1.5 s window).

Pure Python stdlib — no ``numpy``, ``sox``, or ``ffmpeg`` required. The output
is regenerated on every run so no binary fixture needs to live in git.
"""

from __future__ import annotations

import io
import math
import struct
import wave

SAMPLE_RATE = 16000  # voice server expects 16 kHz mono s16le


def synthetic_speech_wav() -> bytes:
    """Return a WAV-formatted byte string suitable for driving the voice WS.

    See module docstring for the waveform breakdown. The returned bytes include
    the 44-byte WAV header; callers streaming raw PCM over the websocket must
    skip the header (see ``tests/test_voice.py`` for the reference pattern).
    """
    silence_pre = _silence_pcm(2.0)
    tone = _tone_pcm(0.8, freq_hz=440.0, amplitude=0.15)
    silence_post = _silence_pcm(1.0)
    pcm = silence_pre + tone + silence_post

    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)  # s16le = 2 bytes/sample
        w.setframerate(SAMPLE_RATE)
        w.writeframes(pcm)
    return buf.getvalue()


def _silence_pcm(seconds: float) -> bytes:
    return b"\x00\x00" * int(SAMPLE_RATE * seconds)


def _tone_pcm(seconds: float, *, freq_hz: float, amplitude: float) -> bytes:
    n_samples = int(SAMPLE_RATE * seconds)
    scale = int(amplitude * 32767)
    two_pi_f_over_sr = 2.0 * math.pi * freq_hz / SAMPLE_RATE
    return b"".join(
        struct.pack("<h", int(scale * math.sin(two_pi_f_over_sr * i)))
        for i in range(n_samples)
    )
