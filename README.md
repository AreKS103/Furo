# Project Furo

A lightweight, local, and free alternative to Wispr Flow. Real-time voice-to-text dictation globally across your OS, powered by faster-whisper (CTranslate2) with Silero VAD filtering.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        app.py (Orchestrator)                 │
│                                                              │
│  ┌──────────┐   hold key   ┌──────────┐   raw PCM           │
│  │  pynput  │ ───────────► │ PyAudio  │ ──────────┐         │
│  │  Hotkey  │   release    │ Recorder │           │         │
│  │ Listener │ ─────┐      └──────────┘           ▼         │
│  └──────────┘      │                        ┌─────────┐     │
│                     │                        │ Silero  │     │
│                     │                        │  VAD    │     │
│                     │                        └────┬────┘     │
│                     │                             │ speech   │
│  ┌──────────┐      │      ┌──────────────┐       │ only     │
│  │  pynput  │ ◄────┴───── │ faster-whisper│ ◄────┘         │
│  │  Typer   │    text     │  (CTranslate2) │                │
│  └──────────┘              └──────────────┘                 │
└──────────────────────────────────────────────────────────────┘
```

### Module Breakdown

| File | Purpose |
|---|---|
| `config.py` | All tunables: audio, VAD, model, hotkey, typing |
| `audio.py` | PyAudio microphone capture with thread-safe buffering |
| `vad.py` | Silero VAD (ONNX) — filters silence from audio stream |
| `transcriber.py` | faster-whisper inference (CTranslate2 backend) |
| `typer.py` | pynput keystroke simulation into the active window |
| `app.py` | Main orchestrator — wires hotkey → record → VAD → transcribe → type |
| `fine_tune.py` | **Isolated** LoRA fine-tuning script (separate venv) |
| `convert_to_ct2.ps1` | PowerShell script to convert fine-tuned model to CT2 |

## Part A: Setup

### 1. Create Production Virtual Environment

```powershell
cd Furo
python -m venv venv
venv\Scripts\activate
pip install -r requirements.txt
```

### 2. PyAudio on Windows

PyAudio requires PortAudio. If `pip install PyAudio` fails:

```powershell
# Option 1: Use pipwin
pip install pipwin
pipwin install pyaudio

# Option 2: Download .whl from https://www.lfd.uci.edu/~gohlke/pythonlibs/#pyaudio
pip install PyAudio‑0.2.14‑cp312‑cp312‑win_amd64.whl
```

### 3. First Run

```powershell
python app.py
```

On first launch, faster-whisper auto-downloads `large-v3-turbo` from HuggingFace (~3 GB). Subsequent starts load from cache (~2 s).

### 4. Usage

| Action | Key |
|---|---|
| **Start dictation** | Hold `F9` |
| **Stop & transcribe** | Release `F9` |
| **Exit** | Press `Escape` |

Change the hotkey in `config.py` → `HOTKEY_NAME`.

## Part B: Main Application

The app follows a strict pipeline:

1. **Hotkey press** → `AudioRecorder.start()` opens a PyAudio callback stream
2. **Hotkey release** → `AudioRecorder.stop()` returns all buffered PCM bytes
3. **VAD filter** → `VoiceActivityDetector.is_speech()` classifies each chunk; only speech frames survive
4. **Transcription** → `Transcriber.transcribe()` feeds speech-only audio to faster-whisper on GPU
5. **Output** → `type_text()` simulates keystrokes via pynput into the focused window

### Why Silero VAD Matters

Without VAD, Whisper hallucinates text during silence (e.g., repeating "[BLANK_AUDIO]", "Thank you.", or phantom sentences). Silero catches 100% of silence frames and typically reduces audio sent to the GPU by 30–70%, slashing inference time proportionally.

### Config Tuning

- **`VAD_THRESHOLD`**: Lower (0.3) = more sensitive; Higher (0.6) = stricter
- **`COMPUTE_TYPE`**: `int8_float16` is ideal for 8 GB VRAM. Use `float16` if you see quality issues
- **`INITIAL_PROMPT`**: Seed the model with your custom vocabulary: `"Furo, CTranslate2, LoRA, pynput"`
- **`TYPING_DELAY`**: Increase to 0.015 if target app drops characters

## Part C: Fine-Tuning (Separate Environment)

### 1. Create Training Virtual Environment

```powershell
python -m venv venv-training
venv-training\Scripts\activate
pip install -r requirements-training.txt
```

### 2. Prepare Dataset

Create a `data/` directory:

```
data/
  metadata.csv
  audio/
    clip_001.wav
    clip_002.wav
```

`metadata.csv`:
```csv
file_name,transcription
audio/clip_001.wav,"Your custom sentence here."
audio/clip_002.wav,"Another example with special vocabulary."
```

**Tips:**
- Record 30–200 clips of yourself speaking your domain vocabulary
- Keep clips under 30 seconds (hard limit in the script to prevent OOM)
- Use 16 kHz mono WAV format
- Include edge cases: technical terms, proper nouns, accented words

### 3. Run Fine-Tuning

```powershell
python fine_tune.py --data_dir ./data --output_dir ./finetuned-whisper
```

**OOM-Prevention Settings (already baked into `fine_tune.py`):**

| Setting | Value | Why |
|---|---|---|
| LoRA rank | 8 | Only ~0.1% of params are trainable |
| Batch size | 2 | Minimal per-step memory |
| Gradient accumulation | 4 | Effective batch of 8 without memory spike |
| FP16 | True | Halves activation memory |
| Gradient checkpointing | True | Recomputes activations instead of storing them |
| Max clip duration | 30s | Caps spectrogram size |
| `use_cache` | False | Required for gradient checkpointing |

### 4. Output

After training completes:
- `finetuned-whisper/lora-adapter/` — LoRA adapter weights only (~20 MB)
- `finetuned-whisper/merged-model/` — Full model with LoRA merged back in

## Part D: CTranslate2 Conversion

Convert the merged model to CTranslate2 format for use with faster-whisper:

### Option 1: PowerShell Script

```powershell
# Activate the training venv (needs ctranslate2 package)
venv-training\Scripts\activate
.\convert_to_ct2.ps1
```

### Option 2: Manual Command

```powershell
ct2-whisper-converter `
    --model ./finetuned-whisper/merged-model `
    --output_dir ./models/furo-whisper-ct2 `
    --quantization int8_float16
```

### Option 3: Python API

```python
import ctranslate2
converter = ctranslate2.converters.TransformersConverter(
    "finetuned-whisper/merged-model"
)
converter.convert(
    output_dir="models/furo-whisper-ct2",
    quantization="int8_float16",
)
```

### Use the Fine-Tuned Model

Update `config.py`:

```python
CT2_MODEL = "./models/furo-whisper-ct2"
```

Then restart the app: `python app.py`

## Troubleshooting

| Issue | Fix |
|---|---|
| `CUDA out of memory` during app | Switch `COMPUTE_TYPE` to `int8` in config.py |
| No audio captured | Check default mic in Windows Sound Settings |
| Typing goes to wrong window | Click the target window before releasing the hotkey |
| Whisper hallucinates on silence | Lower `VAD_THRESHOLD` to 0.3 |
| Slow first transcription | Normal — model loads on first call. Call `transcriber.load_model()` at startup (already done) |
| PyAudio install fails | See PyAudio section above |
| OOM during fine-tuning | Reduce `TRAIN_BATCH_SIZE` to 1, or cut `MAX_AUDIO_LENGTH_S` to 15 |
