"""Voice stack flows — voice-server health, /v1/voice UI, WS upgrade.

The full WAV round-trip is skipped by default because LLaMA-Omni2 cold-starts
can exceed 10s. Enable with ``E2E_VOICE_WAV=1`` to actually drive audio.
"""

from __future__ import annotations

import asyncio
import json
import os

import pytest
import requests
import websockets

from fixtures.voice import synthetic_speech_wav
from helpers import OdinClient
from helpers.services import service_urls


@pytest.mark.required_services("voice")
def test_voice_server_health_reports_model_and_voice() -> None:
    url = service_urls()["voice"]
    resp = requests.get(f"{url.rstrip('/')}/health", timeout=5)
    assert resp.status_code == 200, f"voice /health must be 200, got {resp.status_code}"
    payload = resp.json()
    assert payload.get("model"), "voice health must expose the loaded model name"
    assert payload.get("voice"), "voice health must expose the configured voice persona"
    assert payload.get("status") == "ok", f"voice status must be ok, got {payload.get('status')!r}"


@pytest.mark.required_services("odin")
def test_voice_debug_ui_served(odin_client: OdinClient) -> None:
    url = odin_client._url("/v1/voice/ui")
    resp = requests.get(url, timeout=5)
    if resp.status_code == 404:
        pytest.skip("/v1/voice/ui not exposed on this Odin build (audit listed it as optional)")
    assert resp.status_code == 200, f"/v1/voice/ui must serve debug HTML, got {resp.status_code}"
    body = resp.text.lower()
    # Require BOTH signals. An OR was too weak — "voice service unavailable"
    # error pages would satisfy ``"voice" in body`` while being clearly broken.
    assert "<html" in body, (
        f"/v1/voice/ui must serve an HTML page (got 200 but no <html tag); "
        f"body prefix: {resp.text[:200]!r}"
    )
    assert "voice" in body, (
        "HTML page must reference the word 'voice' — otherwise it's the wrong "
        "page served under the right route"
    )


@pytest.mark.required_services("odin")
def test_voice_websocket_accepts_connection(odin_client: OdinClient) -> None:
    """Assert the WS upgrade succeeds and the server sends a 'ready' frame.

    We do not send audio here — that's gated on E2E_VOICE_WAV=1 because it
    wakes LLaMA-Omni2 and burns GPU for 5-10s.
    """
    ws_url = odin_client._url("/v1/voice").replace("http://", "ws://").replace("https://", "wss://")

    async def _drive() -> dict:
        async with websockets.connect(ws_url, open_timeout=10, close_timeout=2) as ws:
            raw = await asyncio.wait_for(ws.recv(), timeout=5)
            return json.loads(raw)

    # If the server sends a non-JSON first frame, json.loads raises and the
    # test fails loudly — that's the point. The previous ``"raw" in payload``
    # fallback accepted ANY garbage frame (error text, truncated JSON, random
    # bytes) as a valid greeting.
    payload = asyncio.run(_drive())
    assert isinstance(payload, dict), f"first WS frame must be a JSON object; got {type(payload).__name__}"
    kind = payload.get("type") or payload.get("event") or ""
    assert kind in ("ready", "hello", "session"), (
        f"first WS frame must be a known greeting type ('ready'|'hello'|'session'); got {payload!r}"
    )
    if kind == "ready":
        assert payload.get("session_id"), (
            f"'ready' frame must carry a session_id per the WS protocol; got {payload!r}"
        )


@pytest.mark.slow
@pytest.mark.required_services("odin", "voice")
@pytest.mark.skipif(os.environ.get("E2E_VOICE_WAV") != "1", reason="set E2E_VOICE_WAV=1 to drive real audio")
def test_voice_wav_round_trip_produces_transcript(odin_client: OdinClient) -> None:
    """Stream a synthetic tone burst through /v1/voice and verify the VAD
    pipeline advances through ``ready → listening → processing`` and emits a
    ``transcript`` (or structured ``error``) as its terminal frame.

    The fixture is generated in-process (:func:`fixtures.voice.synthetic_speech_wav`)
    — no binary committed to git. Silence alone would be filtered by the VAD
    pre-stage, so the waveform includes a 0.8 s 440 Hz tone above the onset
    threshold.
    """
    ws_url = odin_client._url("/v1/voice").replace("http://", "ws://").replace("https://", "wss://")
    wav_bytes = synthetic_speech_wav()
    # Strip the 44-byte PCM WAV header — the server expects raw s16le frames.
    pcm = wav_bytes[44:]
    frame_bytes = 8192  # 4096 samples × 2 bytes = 256 ms per binary frame

    async def _drive() -> list[dict]:
        async with websockets.connect(ws_url, open_timeout=10, close_timeout=2) as ws:
            # Handshake.
            ready = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
            assert ready.get("type") == "ready" and ready.get("session_id"), (
                f"expected 'ready' with session_id; got {ready!r}"
            )

            # Stream PCM in ~256 ms frames. A brief sleep between sends keeps the
            # server's ring buffer ingest honest (VAD runs on real-time windows,
            # not batched bursts).
            for offset in range(0, len(pcm), frame_bytes):
                await ws.send(pcm[offset:offset + frame_bytes])
                await asyncio.sleep(0.05)
            # Explicit VAD-end signal (the server's silence detector will also
            # fire after the trailing 1 s silence, but this makes the test
            # resilient to clock skew).
            await ws.send(json.dumps({"type": "vad_end"}))

            # Collect response frames. We cap at 20 s total — LLaMA-Omni2
            # cold-start + TTS is ~10-15 s, so 20 s is a generous ceiling.
            loop = asyncio.get_running_loop()
            messages: list[dict] = []
            deadline = loop.time() + 20.0
            while loop.time() < deadline:
                remaining = deadline - loop.time()
                try:
                    raw = await asyncio.wait_for(ws.recv(), timeout=max(remaining, 0.1))
                except asyncio.TimeoutError:
                    break
                if isinstance(raw, (bytes, bytearray)):
                    # Binary TTS audio chunk — not what we're asserting on.
                    continue
                try:
                    messages.append(json.loads(raw))
                except json.JSONDecodeError:
                    continue
                # Terminal states — stop collecting once the pipeline closes out.
                last = messages[-1].get("type")
                if last in ("audio_end", "error"):
                    break
            return messages

    messages = asyncio.run(_drive())
    types = [m.get("type") for m in messages]
    assert "listening" in types, (
        f"VAD never transitioned to 'listening' — the tone burst didn't cross the "
        f"onset threshold or the server's VAD is broken. frames={types!r}"
    )
    assert "processing" in types, (
        f"VAD never reached 'processing' — endpoint detection didn't fire after "
        f"the trailing silence + explicit vad_end. frames={types!r}"
    )
    # Terminal states: either the pipeline returned a transcript/response, or
    # it produced a structured error. A hang (no terminal frame at all) is a
    # pipeline regression.
    terminal_ok = any(t in ("transcript", "response", "audio_end", "error") for t in types)
    assert terminal_ok, (
        f"voice pipeline advanced past VAD but never produced a terminal frame "
        f"(transcript/response/audio_end/error); got types={types!r}"
    )
