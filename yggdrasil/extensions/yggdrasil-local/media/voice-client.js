/* Yggdrasil voice-client.js — Sprint 062 P4.
   Manages PTT voice session:
   - AudioContext + MediaStream → AudioWorkletNode → WebSocket binary frames
   - Handles JSON events: ready, listening, processing, transcript, response,
     audio_start, audio_end, error
   - Handles binary PCM TTS frames (24kHz s16le) → AudioContext playback
   - PTT state machine: idle → recording → processing → playback → idle
   - Session resume: stores session_id in sessionStorage, reconnects within 60s
   Injected into chat webview when yggdrasil.voice.enabled = true. */

'use strict';

(function initVoiceClient() {
  const vscode = typeof acquireVsCodeApi !== 'undefined' ? acquireVsCodeApi() : null;

  // ── Config from body dataset ───────────────────────────────────
  const ODIN_URL     = document.body.dataset.odinUrl ?? 'http://10.0.65.9:8080';
  const VOICE_ENABLED = document.body.dataset.voiceEnabled === 'true';
  const TTS_ENABLED  = document.body.dataset.ttEnabled !== 'false';

  if (!VOICE_ENABLED) return;

  // Derive WebSocket URL from odinUrl (ws:// vs wss://)
  const wsBase = ODIN_URL.replace(/^http/, 'ws');
  const WS_URL = `${wsBase}/v1/voice`;

  // ── State ──────────────────────────────────────────────────────
  let ws = null;
  let audioCtx = null;
  let mediaStream = null;
  let workletNode = null;
  let sourceNode = null;
  let sessionId = sessionStorage.getItem('ygg_voice_session_id') ?? null;
  let lastConnectTime = 0;
  let pttActive = false;
  let ttsBuffers = [];
  let ttsPlaying = false;
  let reconnectTimer = null;
  let connectAttempts = 0;
  const MAX_RECONNECT_MS = 30_000;

  const SESSION_RESUME_WINDOW_MS = 60_000;

  // ── Mic button wiring ──────────────────────────────────────────
  const micBtn = document.getElementById('mic-btn');
  if (micBtn) {
    micBtn.style.display = '';
    micBtn.addEventListener('mousedown', onPttStart);
    micBtn.addEventListener('mouseup',   onPttEnd);
    micBtn.addEventListener('touchstart', onPttStart, { passive: true });
    micBtn.addEventListener('touchend',   onPttEnd,   { passive: true });
    micBtn.title = 'Hold for voice (push-to-talk)';
  }

  // Extension host posts voice.toggle to trigger PTT programmatically
  window.addEventListener('message', (e) => {
    if (e.data?.type === 'voice.toggle') {
      if (pttActive) onPttEnd();
      else onPttStart();
    }
  });

  // ── PTT handlers ───────────────────────────────────────────────
  async function onPttStart() {
    if (pttActive) return;
    pttActive = true;
    if (micBtn) { micBtn.style.color = 'var(--red)'; micBtn.title = 'Recording…'; }

    await ensureConnected();
    await ensureAudio();
    if (workletNode) workletNode.port.postMessage('start');
  }

  function onPttEnd() {
    if (!pttActive) return;
    pttActive = false;
    if (micBtn) { micBtn.style.color = ''; micBtn.title = 'Hold for voice'; }
    if (workletNode) workletNode.port.postMessage('stop');

    // Signal VAD end
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'vad_end' }));
    }
  }

  // ── WebSocket management ────────────────────────────────────────
  async function ensureConnected() {
    if (ws && ws.readyState === WebSocket.OPEN) return;

    return new Promise((resolve, reject) => {
      const now = Date.now();
      const canResume = sessionId && (now - lastConnectTime) < SESSION_RESUME_WINDOW_MS;

      ws = new WebSocket(WS_URL);
      ws.binaryType = 'arraybuffer';

      ws.onopen = () => {
        connectAttempts = 0;
        lastConnectTime = Date.now();
        // Send handshake
        if (canResume) {
          ws.send(JSON.stringify({ type: 'resume', session_id: sessionId }));
        } else {
          ws.send(JSON.stringify({ type: 'hello' }));
        }
        resolve(undefined);
      };

      ws.onmessage = handleWsMessage;

      ws.onerror = (err) => {
        console.warn('[voice] WebSocket error', err);
        reject(err);
      };

      ws.onclose = () => {
        if (pttActive) scheduleReconnect();
      };
    });
  }

  function scheduleReconnect() {
    if (reconnectTimer) return;
    const delay = Math.min(1000 * Math.pow(2, connectAttempts++), MAX_RECONNECT_MS);
    reconnectTimer = setTimeout(async () => {
      reconnectTimer = null;
      try { await ensureConnected(); } catch { /* will retry */ }
    }, delay);
  }

  // ── Message handling ────────────────────────────────────────────
  function handleWsMessage(ev) {
    if (ev.data instanceof ArrayBuffer) {
      // Binary PCM TTS frame (24kHz s16le)
      if (TTS_ENABLED) enqueueTts(ev.data);
      return;
    }

    let msg;
    try { msg = JSON.parse(ev.data); } catch { return; }

    switch (msg.type) {
      case 'ready':
        if (msg.session_id) {
          sessionId = msg.session_id;
          sessionStorage.setItem('ygg_voice_session_id', sessionId);
        }
        break;
      case 'listening':
        setStatus('Listening…');
        break;
      case 'processing':
        setStatus('Processing…');
        break;
      case 'transcript':
        if (msg.text) insertTranscript(msg.text);
        break;
      case 'response':
        setStatus('');
        break;
      case 'audio_start':
        ttsPlaying = false;
        ttsBuffers = [];
        break;
      case 'audio_end':
        playTtsBuffers();
        break;
      case 'error':
        console.error('[voice] server error:', msg.message);
        setStatus('Voice error: ' + (msg.message ?? 'unknown'));
        break;
    }
  }

  // ── Audio setup ─────────────────────────────────────────────────
  async function ensureAudio() {
    if (workletNode) return;

    audioCtx = new AudioContext({ sampleRate: 48000 });
    mediaStream = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });
    sourceNode  = audioCtx.createMediaStreamSource(mediaStream);

    // Load worklet — URI is stored as data-voice-worklet on body
    const workletUri = document.body.dataset.voiceWorkletUri ?? '';
    await audioCtx.audioWorklet.addModule(workletUri);
    workletNode = new AudioWorkletNode(audioCtx, 'voice-resampler');

    workletNode.port.onmessage = (e) => {
      if (!pttActive) return;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(e.data);
      }
    };

    sourceNode.connect(workletNode);
  }

  // ── TTS playback ─────────────────────────────────────────────────
  function enqueueTts(buf) {
    ttsBuffers.push(buf);
  }

  async function playTtsBuffers() {
    if (ttsPlaying || ttsBuffers.length === 0) return;
    ttsPlaying = true;

    if (!audioCtx) audioCtx = new AudioContext({ sampleRate: 24000 });

    // Combine all PCM buffers
    const totalBytes = ttsBuffers.reduce((s, b) => s + b.byteLength, 0);
    const combined = new Int16Array(totalBytes / 2);
    let off = 0;
    for (const buf of ttsBuffers) {
      combined.set(new Int16Array(buf), off);
      off += buf.byteLength / 2;
    }
    ttsBuffers = [];

    // Convert int16 to float32
    const float32 = new Float32Array(combined.length);
    for (let i = 0; i < combined.length; i++) float32[i] = combined[i] / 32768;

    const audioBuffer = audioCtx.createBuffer(1, float32.length, 24000);
    audioBuffer.copyToChannel(float32, 0);

    const source = audioCtx.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(audioCtx.destination);
    source.onended = () => { ttsPlaying = false; };
    source.start();
  }

  // ── UI helpers ──────────────────────────────────────────────────
  function setStatus(text) {
    const el = document.getElementById('statusline-mode');
    if (el) el.textContent = text || (pttActive ? 'RECORDING' : 'IDLE');
  }

  function insertTranscript(text) {
    const input = document.getElementById('input');
    if (!input) return;
    input.value = (input.value ? input.value + ' ' : '') + text;
    input.dispatchEvent(new Event('input'));
  }
})();
