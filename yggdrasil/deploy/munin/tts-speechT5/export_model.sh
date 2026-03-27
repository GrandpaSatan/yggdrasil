#!/usr/bin/env bash
# Export SpeechT5 to OpenVINO IR format for NPU inference.
# Run this once on Munin to prepare the model.
set -e

VENV="/opt/yggdrasil/deploy/munin/tts-speechT5/.venv"
MODEL_DIR="/opt/yggdrasil/deploy/munin/tts-speechT5/speecht5_tts"

echo "Exporting SpeechT5 to OpenVINO IR..."
$VENV/bin/optimum-cli export openvino \
    --model microsoft/speecht5_tts \
    --model-kwargs '{"vocoder": "microsoft/speecht5_hifigan"}' \
    "$MODEL_DIR"

echo "Downloading default speaker embedding..."
$VENV/bin/python3 -c "
from datasets import load_dataset
import numpy as np
ds = load_dataset('Matthijs/cmu-arctic-xvectors', split='validation')
# Use a male speaker embedding (index 7306 is commonly used)
embedding = np.array(ds[7306]['xvector'], dtype=np.float32)
embedding.tofile('$MODEL_DIR/speaker_embedding.bin')
print(f'Saved speaker embedding: {len(embedding)} values')
"

echo "Model exported to $MODEL_DIR"
echo "Speaker embedding at $MODEL_DIR/speaker_embedding.bin"
ls -la "$MODEL_DIR/"
