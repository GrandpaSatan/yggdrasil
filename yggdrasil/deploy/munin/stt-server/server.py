"""SenseVoice STT Server — FastAPI wrapper for FunAudioLLM/SenseVoiceSmall"""

import io
import time
import logging
import numpy as np
import soundfile as sf
from fastapi import FastAPI, UploadFile, File, Request
from fastapi.responses import JSONResponse

log = logging.getLogger("stt")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")

app = FastAPI(title="SenseVoice STT")

model = None

@app.on_event("startup")
async def load_model():
    global model
    from funasr import AutoModel
    log.info("loading SenseVoice-Small...")
    model = AutoModel(
        model="iic/SenseVoiceSmall",
        trust_remote_code=True,
        device="cpu",
        disable_update=True,
    )
    log.info("SenseVoice-Small loaded")

@app.get("/health")
async def health():
    return {"status": "ok", "model": "SenseVoiceSmall"}

@app.post("/api/v1/stt")
async def stt(request: Request, file: UploadFile = File(None)):
    t0 = time.time()
    content_type = request.headers.get("content-type", "")
    if file is not None and "multipart" in content_type:
        audio_bytes = await file.read()
    else:
        # Raw PCM/WAV bytes from Odin (application/octet-stream)
        audio_bytes = await request.body()

    # Try reading as WAV first, fall back to raw 16-bit PCM at 16kHz
    try:
        audio_data, sr = sf.read(io.BytesIO(audio_bytes))
    except Exception:
        audio_data = np.frombuffer(audio_bytes, dtype=np.int16).astype(np.float32) / 32768.0
        sr = 16000

    if len(audio_data.shape) > 1:
        audio_data = audio_data.mean(axis=1)

    result = model.generate(
        input=audio_data,
        cache={},
        language="auto",
        use_itn=True,
        batch_size_s=60,
    )

    text = ""
    if result and len(result) > 0:
        text = result[0].get("text", "")
    # Strip SenseVoice metadata tags: <|en|><|EMO_UNKNOWN|><|Speech|> etc.
    import re
    text = re.sub(r"<\|[^|]*\|>", "", text).strip()

    elapsed = time.time() - t0
    log.info("transcribed in %.2fs: %s", elapsed, text[:100])
    return JSONResponse({"text": text, "duration": elapsed})

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=9097)
