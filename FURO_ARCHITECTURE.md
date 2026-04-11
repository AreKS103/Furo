# Project Furo — Complete Architecture & Logic Reference

> **Purpose**: Feed this document to an external AI for bug/improvement analysis.  
> **Date**: April 9, 2026 | **Stack**: Tauri v2 (Rust) + React 19 + TypeScript + Python FastAPI sidecar  
> **Hardware target**: Windows 10/11, NVIDIA RTX 2070 (8 GB VRAM), 16 GB RAM

## INITIALIZE COMMANDS
```powershell
# Terminal 1: Python backend
python server.py

# Terminal 2: Tauri dev
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
npx tauri dev
---

## 1. What Furo Is

A **local, free, offline** voice-to-text dictation tool — a Wispr Flow alternative. The user presses a global hotkey in any application, speaks, and the transcribed text is typed into the active window. No cloud, no subscription, no telemetry.

---

## 2. High-Level Architecture

```
┌─────────────────────────────────────────────────┐
│                  Tauri v2 Shell                 │
│  (Rust binary — tray, windows, sidecar mgmt)    │
├─────────┬───────────────────────────┬───────────┤
│ Main    │  Floating Widget          │  System   │
│ Window  │  (frameless, transparent, │  Tray     │
│ 480×640 │   always-on-top pill)     │  (3 recent│
│         │   skip_taskbar + no focus │   + copy) │
├─────────┴───────────────────────────┴───────────┤
│         React 19 / TypeScript / Tailwind        │
│         (Vite 6.3, @tauri-apps/* plugins)       │
├─────────────────────────────────────────────────┤
│            WebSocket ws://127.0.0.1:9876/ws     │
│            REST      http://127.0.0.1:9876/api  │
├─────────────────────────────────────────────────┤
│         Python FastAPI Sidecar (server.py)      │
│  ┌─────────┐ ┌─────────┐ ┌────────┐ ┌────────┐  │
│  │ pynput  │ │ PyAudio │ │ Silero │ │ faster │  │
│  │(hotkey) │ │ (mic)   │ │  VAD   │ │whisper │  │
│  └────┬────┘ └────┬────┘ └───┬────┘ └───┬────┘  │
│       │           │          │           │      │
│       └─► _hotkey_queue ─► _hotkey_worker ──►  │
│             (enqueue)       (daemon thread)     │
│             start/stop      modifier tracking   │
│             recording       key matching        │
│                               ↓                 │
│                          typer.py               │
│                    (pyperclip + SendInput Ctrl+V)│
└─────────────────────────────────────────────────┘
```

---

## 3. Data Flow — Dictation Lifecycle

1. **Hotkey pressed** → pynput callback enqueues `("key_press", key, injected)` to `_hotkey_queue` and returns **instantly** (< 1 µs — no blocking on hook thread)
2. **Queue worker thread** (`_hotkey_worker`) dequeues the event, tracks modifiers, matches hold or hands-free trigger, calls `_handle_hold_press()` or `_handle_handsfree_press()`
3. **HWND captured** → `capture_target()` calls `GetForegroundWindow()` + `GetGUIThreadInfo()` via ctypes → stored as `(_target_parent_hwnd, _target_child_hwnd)`
4. **Recording starts** → `_do_start(mode)` acquires `_hotkey_lock`, sets `_recording_mode`, opens mic stream via `AudioRecorder.start()`
5. **Mic stream** → PyAudio callback at 16 kHz mono int16, 512 frames/buffer (~32 ms)
6. **Real-time VAD + volume** → each chunk passes through Silero VAD; speech chunks accumulate in `_speech_buffer`; RMS level broadcast as `{"type":"volume","level":0.xxx}`
7. **Hotkey released** → queue worker calls `_handle_hold_release()` → `_do_stop()` → `_stop_and_process()`
8. **Recording stops** → `AudioRecorder.stop()`, broadcasts `"processing"` status, schedules `process_audio()` on asyncio event loop
9. **Whisper inference** → `process_audio()` drains speech buffer, calls `transcriber.transcribe()` in thread pool (GPU-bound)
10. **DSP conditioning** → Pedalboard: 300 Hz highpass → 2500 Hz high-shelf +5 dB → compressor (-20 dB, 3:1) — runs before Whisper
11. **Post-processing** → regex filler stripping (`uh`, `um`, `like`), stuttered-double removal, space collapse
12. **Broadcast** → `{"type":"transcription","text":"..."}` to all WS clients
13. **Focus restoration** → `typer._restore_focus(parent_hwnd)`:
    - `IsWindow(hwnd)` validation → if dead, clipboard-only fallback + broadcast `{"type":"error"}`
    - `AttachThreadInput` to **both** foreground thread AND target thread
    - `AllowSetForegroundWindow(ASFW_ANY)` → `IsIconic` check → `SetForegroundWindow` → `BringWindowToTop` → `SetFocus`
    - `finally` block always detaches threads (prevents deadlock)
14. **Paste** → `pyperclip.copy(text)` puts text on clipboard, then `_send_ctrl_v()` simulates physical Ctrl+V via `SendInput` (4-event sequence: Ctrl↓ V↓ V↑ Ctrl↑)
15. **Fallback** → if focus restore failed: text was already copied by pyperclip, server returns `False`, broadcasts error, frontend fires OS toast notification

---

## 4. File-by-File Logic

### Python Backend

| File | Lines | Purpose |
|---|---|---|
| `server.py` | ~730 | FastAPI + WS + Queue-based hotkey orchestrator + recording pipeline + GPU preflight |
| `typer.py` | ~270 | pyperclip clipboard + Win32 focus lock + SendInput Ctrl+V |
| `transcriber.py` | ~180 | faster-whisper wrapper + CUDA enforcement + DSP conditioning + filler stripping |
| `audio.py` | ~160 | PyAudio recorder, thread-safe frame buffer, RMS callback |
| `vad.py` | ~100 | Silero VAD wrapper (ONNX), per-chunk speech classification |
| `config.py` | ~55 | All tunables (audio, VAD, whisper, typing, paths) |
| `settings_store.py` | ~70 | Thread-safe JSON settings in `%APPDATA%/Furo/settings.json` |

### Frontend (React + TypeScript)

| File | Purpose |
|---|---|
| `src/main.tsx` | Routing by `?window=widget` query param → `<App>` or `<FloatingWidget>` |
| `src/App.tsx` | Theme state + dark mode class toggle, renders `<Dashboard>` |
| `src/components/Dashboard.tsx` | Settings tab (mic select, hotkey rebind) + History tab (Notion-style cards) + status bar |
| `src/components/FloatingWidget.tsx` | Always-on-top transparent pill, audio visualizer bars, multi-monitor repositioning |
| `src/hooks/useSocket.ts` | WebSocket connection manager, auto-reconnect, exposes `{state, message, lastText, lastError, settings, volume}` |
| `src/hooks/useHistory.ts` | `@tauri-apps/plugin-store` backed history, 48-hour auto-purge, emits `update-tray` events |

### Rust (Tauri Shell)

| File | Purpose |
|---|---|
| `src-tauri/src/lib.rs` | App setup: widget window, system tray, tray menu events, sidecar spawn, close-to-tray, `update-tray` listener |
| `src-tauri/src/main.rs` | Entry point, calls `app_lib::run()` |

---

## 5. Hotkey System — Detailed Logic

**Dual-mode**: Two independent configurable hotkey combos — one for "hold to record", one for "hands-free toggle".

```
Hotkey string format: "ctrl+win+space", "mouse4", "ctrl+mouse5", "vk166"
```

### Queue-Based Architecture (LowLevelHooksTimeout fix)

Windows gives the low-level keyboard hook thread a strict ~200 ms budget. Any blocking code in the callback will cause Windows to kill the hook (LowLevelHooksTimeout), freezing ALL keyboard events system-wide.

**Solution: the pynput callbacks do exactly one thing — enqueue and return.**

```
pynput thread    →  _on_key_press     →  _hotkey_queue.put_nowait(("key_press", key, injected))
                 →  _on_key_release   →  _hotkey_queue.put_nowait(("key_release", key, injected))
                 →  _on_mouse_click   →  _hotkey_queue.put_nowait(("mouse_click", button, pressed))

_hotkey_worker   ←  queue.get(timeout=0.5)
(daemon thread)      ↓
                     _worker_handle_key_press / _worker_handle_key_release / _worker_handle_mouse
                     ↓
                     _handle_hold_press / _handle_hold_release / _handle_handsfree_press
                     ↓
                     _do_start(mode) / _do_stop()   [both acquire _hotkey_lock]
```

- `suppress=False` is explicit on both `pynput_kb.Listener` and `pynput_mouse.Listener` — Furo never swallows system keystrokes
- The worker thread starts in `start_hotkey_listener()` and is gracefully joined in `stop_hotkey_listener()` via `_hotkey_worker_stop` Event
- **Modifier cleanup failsafe**: if any exception occurs in the worker while `_recording_mode == "none"`, `_active_modifiers.clear()` resets modifier state to prevent stuck-Ctrl syndrome

### Parsing (`_parse_hotkey_combo`)
- Splits on `+`, last token = trigger, preceding tokens = modifier set
- Trigger can be: pynput Key enum, single char KeyCode, `vk<int>` for Razer/macro keys, or `mouse3`/`mouse4`/`mouse5`
- Single-character triggers resolved to vk-based `KeyCode` via `VkKeyScanW` for modifier-proof matching

### Modifier Tracking
- `_active_modifiers: set[str]` — mutated **only** by `_hotkey_worker` thread (no concurrent mutation)
- Maps `Key.ctrl_l`/`Key.ctrl_r` → `"ctrl"`, `Key.cmd_l`/`Key.cmd_r` → `"win"`, etc.
- Injected keystrokes (pynput `injected=True`) are silently discarded to prevent Furo's own SendInput Ctrl+V from re-triggering hotkeys

### Press/Release Flow (inside worker)
```
_worker_handle_key_press(key):
  if modifier → add to _active_modifiers, return
  if hold trigger key matches AND required_modifiers ⊆ _active_modifiers → _handle_hold_press()
  if handsfree trigger key matches AND required_modifiers ⊆ _active_modifiers → _handle_handsfree_press()

_worker_handle_key_release(key):
  if modifier → discard from _active_modifiers
    if mode=="hold" and required modifiers broke → _handle_hold_release()
  if trigger key release AND mode=="hold" → _handle_hold_release()

_worker_handle_mouse(button, pressed):
  if button name matches hold trigger AND pressed → _handle_hold_press()
  if button name matches handsfree trigger AND pressed → _handle_handsfree_press()
  if button name matches hold trigger AND released AND mode=="hold" → _handle_hold_release()
```

### Trigger Handlers (called from worker thread)
- `_handle_hold_press()` → calls `_do_start("hold")` directly (no thread spawn needed — already on worker thread)
- `_handle_hold_release()` → calls `_do_stop()` directly
- `_handle_handsfree_press()` → toggles: `_do_stop()` if recording, else `_do_start("handsfree")`

### Mouse Button Hotkeys
- `pynput.mouse.Listener` runs alongside keyboard listener always
- Only mouse3/mouse4/mouse5 (middle, XButton1, XButton2) are recognized as possible triggers

### Frontend Rebind
- `keydown` handler captures keyboard combos including `vk<keyCode>` for Unidentified keys
- `mousedown` handler captures buttons ≥ 1 (skips left/right click), maps to `mouse3`/`mouse4`/`mouse5`
- Sends combo string to `PUT /api/settings` → server calls `start_hotkey_listener()` to stop/restart listeners and worker thread

---

## 6. Win32 Focus Lock & Text Injection — Detailed Logic

### Problem
User presses hotkey in App A, speaks for 2 seconds, clicks App B during processing → text would type into App B.

### Old Approach (removed)
`_set_clipboard_text()` used `OpenClipboard` / `GlobalAlloc` / `SetClipboardData(CF_UNICODETEXT)` / `CloseClipboard` manually — a 5-step Win32 lifecycle that caused `0x0000000000000000 access violation` crashes in Electron, browser sandboxes, and UWP apps when the clipboard was locked by another process. Text injection used WM_PASTE (0x0302) or WM_CHAR (0x0102), which many modern apps ignore or reject.

### Current Approach (`typer.py`)

#### Clipboard — pyperclip
- `pyperclip.copy(text)` replaces the entire Win32 clipboard lifecycle
- Handles its own locking; safe from access violations
- Text is placed on clipboard **before** focus restoration, so it's always available even if focus fails

#### Focus Restoration — `_restore_focus(target_hwnd)`
1. `IsWindow(hwnd)` — bail out if window is gone → return `False`
2. `AttachThreadInput(our_tid, fg_tid, True)` — attach to current foreground thread
3. `AttachThreadInput(our_tid, target_tid, True)` — attach to target thread (needed for cross-process targets)
4. `IsIconic(hwnd)` → `ShowWindow(SW_RESTORE)` if minimized
5. `AllowSetForegroundWindow(0xFFFFFFFF)` → `SetForegroundWindow` → `BringWindowToTop` → `SetFocus`
6. `finally` block **always** detaches both attachments — prevents thread attachment leaks on exception

#### Paste — SendInput Ctrl+V
- Hardware-level keystroke simulation via `ctypes.windll.user32.SendInput`
- 4-event sequence: `VK_CONTROL↓`, `VK_V↓`, `VK_V↑`, `VK_CONTROL↑`
- No dependency on pynput for paste — no risk of injected event re-triggering hotkeys
- `time.sleep(0.05)` between focus restore and SendInput to allow the window to settle

#### `capture_target()`
- Called at hotkey press (before recording starts), not at paste time
- `GetForegroundWindow()` → parent HWND
- `GetGUIThreadInfo(tid)` → `hwndFocus` (the actual text input child element within the window)
- Returns `(parent_hwnd, child_hwnd)` — both stored on server; `type_text()` uses parent for focus, child was reserved for future child-level focus (currently uses parent)

### Safe Fallback (window closed during recording)
- `IsWindow(hwnd)` returns `False` → text already on clipboard via pyperclip → `type_text()` returns `False`
- `server.py` broadcasts `{"type":"error","message":"Target window was closed — text copied to clipboard."}`
- Frontend `useSocket.ts` populates `lastError` → `Dashboard.tsx` fires `new Notification("Furo", {body: lastError})`

### Error Handling in `_restore_focus`
- If ANY Win32 call throws → logs warning, returns `True` (best-effort: proceed and hope current window is correct)
- If thread detach fails → silently caught (defensive, prevents cascading)

---

## 7. Audio Pipeline — Details

### Recording (`audio.py`)
- `PyAudio` callback stream, `paInt16`, 16 kHz, mono, 512 frames/buffer (~32 ms)
- Frames stored in `deque[bytes]` protected by `threading.Lock`
- RMS volume callback: `np.sqrt(np.mean(samples**2))`, normalized by dividing by 3000.0, clamped to [0, 1], throttled to 50 ms

### VAD (`vad.py`)
- Silero VAD v5 via `silero-vad` package, ONNX backend (not PyTorch inference)
- `load_silero_vad(onnx=True)` — downloads ~1.5 MB model on first run
- Threshold: 0.45 speech probability (configurable via settings)
- Stateful (recurrent) — `reset()` called at start of each recording session
- Chunk size: 512 int16 samples = 1024 bytes = 32 ms
- Only speech chunks are accumulated in `_speech_buffer` and forwarded to Whisper

### Streaming VAD Pipeline
- VAD runs **concurrently during recording** (not post-processing) via `_on_audio_chunk` callback
- `_speech_buffer: deque[bytes]` accumulates speech frames in real-time, protected by `_speech_buffer_lock`
- When recording stops, `process_audio()` drains the pre-filtered buffer — Whisper only ever sees speech-only audio

### DSP Conditioning (`transcriber.py`)
- `pedalboard` library (optional): `HighpassFilter(300 Hz)` → `HighShelfFilter(2500 Hz, +5 dB)` → `Compressor(-20 dB, 3:1)`
- Runs on float32 audio before Whisper — helps with quiet/whispered speech

### Transcription (`transcriber.py`)
- Model: `deepdml/faster-whisper-large-v3-turbo-ct2` (auto-downloaded from HuggingFace)
- Quantization: `int8_float16` (INT8 weights + FP16 activations)
- `beam_size=1` (greedy), `without_timestamps=True`, `condition_on_previous_text=False`
- `initial_prompt`: biased toward programming vocabulary (FastAPI, React, TypeScript, etc.)
- Model loaded **once at startup** via `load_model()` in FastAPI `lifespan` — stays warm in VRAM; no cold start on first use
- **No silent CPU fallback**: `load_model()` checks `ctranslate2.get_supported_compute_types("cuda")` and raises `RuntimeError` if CUDA is unavailable
- Post-processing: regex filler removal (`\b(uh+|um+|er+|ah+|hm+|hmm+|like,?)\b`), stuttered doubles (`\b(\w+)\s+\1\b` → `\1`), space collapse

---

## 8. Frontend Architecture

### Floating Widget (`FloatingWidget.tsx`)
- **Window config**: frameless, transparent, always-on-top, skip-taskbar, `focused(false)`, 200×48 logical pixels
- **States**: idle (10×40 px pill), expanded on hover or active (20×80 px), visualizer bars visible
- **Visualizer**: 10 bars, `requestAnimationFrame` loop, smooth volume via lerp (30%/frame), volume multiplier 3.0, base scale 0.15 when recording
- **Multi-monitor**: Polls `cursorPosition()` + `availableMonitors()` every 500 ms, repositions to bottom-center of the monitor containing the cursor
- **Focus safety**: `tauriShow()` shows window but does NOT call `setFocus()`

### Dashboard (`Dashboard.tsx`)
- **Tabs**: Settings | History
- **Settings tab**: Mic dropdown (filtered, deduplicated), hotkey rebind (keyboard + mouse + macro keys)
- **History tab**: Notion-style cards with relative timestamps, copy button per entry, "Clear All" button
- **Status footer**: Color-coded badge (emerald=ready, red+pulse=recording, amber+pulse=processing, blue+pulse=loading, gray=disconnected)
- **Theme**: Dark/light toggle, persisted to backend

### WebSocket Hook (`useSocket.ts`)
- Auto-reconnect on disconnect (2 s delay)
- Parses `ServerMessage` union: `status` | `transcription` | `settings` | `volume` | `error`
- Exposes: `state`, `message`, `lastText`, `lastError`, `settings`, `volume`
- Volume resets to 0 when state !== "recording"

### History Hook (`useHistory.ts`)
- **Storage**: `@tauri-apps/plugin-store` → `dictation-history.json`
- **Purge**: On init, filters entries older than 48 hours, persists the cleaned list
- **Tray sync**: After every save/clear, emits `"update-tray"` Tauri event with top 3 texts

---

## 9. Rust Layer (`lib.rs`)

### Managed State
- `SidecarChild(Mutex<Option<CommandChild>>)` — holds Python process handle
- `RecentTexts(Mutex<Vec<String>>)` — top 3 dictation texts for tray quick-copy

### Widget Window
- Created in `setup()`: frameless, transparent, always-on-top, skip-taskbar, `focused(false)`
- Position: bottom-center of primary monitor minus 60px from bottom edge

### System Tray
- Built with `TrayIconBuilder::with_id("main-tray")`
- Menu events: `recent_0/1/2` → copies text via `arboard::Clipboard`, `open` → shows main window, `quit` → kills sidecar + exits
- `app.listen("update-tray")` → deserializes `Payload{texts}` → updates `RecentTexts` state → calls `rebuild_tray_menu()`

### Sidecar
- `#[cfg(not(debug_assertions))]` — only in release builds
- `app.shell().sidecar("furo-backend")` → spawns compiled Python exe
- Stdout/stderr piped to console with `[sidecar]` prefix
- Killed on `quit` tray action

### Close-to-Tray
- `on_window_event` hook: if main window close requested → `api.prevent_close()` + `window.hide()`

---

## 10. Settings System

### Backend (`settings_store.py`)
- File: `%APPDATA%/Furo/settings.json`
- Defaults: `microphone=""`, `model="deepdml/faster-whisper-large-v3-turbo-ct2"`, `hotkey_hold="ctrl+win+space"`, `hotkey_handsfree="ctrl+win+f"`, `theme="dark"`, `language="en"`, `compute_type="int8_float16"`, `vad_threshold=0.45`
- Thread-safe with `threading.Lock`, immediate write-through on every `set()`/`update()`

### Frontend Sync
- On WS connect, server sends `{"type":"settings","data":{...}}`
- `PUT /api/settings {key: value}` → updates store → broadcasts new settings to all clients
- Side-effect: if `hotkey_hold` or `hotkey_handsfree` changed, `start_hotkey_listener()` stops and restarts listeners + worker thread with new combos

---

## 11. Microphone Filtering

### Exclude Keywords (`server.py`)
```python
_MIC_EXCLUDE_KEYWORDS = {
    "stereo mix", "what u hear", "wave out", "loopback",
    "monitor of", "virtual", "video", "camera", "webcam",
    "screen capture", "obs", "voicemeeter",
}
```
- `GET /api/microphones` iterates PyAudio devices, skips: channels ≤ 0, name contains exclude keyword, duplicate names (same device across multiple host APIs)

---

## 12. CORS & Security

- CORS: `allow_origins=["*"]` — intentional for local-only server on 127.0.0.1
- Tauri CSP: `null` (disabled) — allows loading Google Fonts via CDN link in CSS
- Capabilities: `core:default`, `core:window:*`, `core:event:default`, `shell:allow-spawn`, `shell:allow-kill`, `store:default`
- Server listens only on `127.0.0.1:9876` — not exposed to network

---

## 13. GPU Preflight & CUDA Initialization

### DLL Path Registration (`server.py`, runs at import time before any CUDA import)
```python
for sp in site.getsitepackages():
    for pkg in (sp/nvidia).iterdir():
        bin_dir = pkg/"bin"
        os.add_dll_directory(bin_dir)        # Python 3.8+ DLL policy
        os.environ["PATH"] = bin_dir + os.pathsep + os.environ["PATH"]
```
Finds all `site-packages/nvidia/*/bin/` directories (cuBLAS, cuDNN, etc.) and registers them so Windows `LoadLibrary` finds them before any CTranslate2 import.

### `_gpu_preflight()` (runs on startup)
1. `ctranslate2.get_supported_compute_types("cuda")` — primary CUDA check (independent of PyTorch)
2. Verifies `config.COMPUTE_TYPE` (`int8_float16`) is in supported types
3. Queries PyTorch CUDA as supplementary info (GPU name, VRAM) — failure here is non-fatal since torch is CPU-only for VAD
4. Crashes hard with descriptive error if CTranslate2 cannot access CUDA — no silent fallback

### Why Not `torch.cuda.is_available()`
PyTorch in this project is deliberately installed CPU-only (smaller wheel, only used for Silero VAD tensor operations). `torch.cuda.is_available()` would return `False` even on a CUDA-capable machine. CTranslate2 ships its own CUDA runtime via `nvidia-cublas-cu12` and `nvidia-cudnn-cu12`.

---

## 14. Build & Dependencies

### Python (venv)
```
faster-whisper, ctranslate2, silero-vad, torch (CPU only, for VAD tensor ops),
pyaudio, pynput 1.8.x, numpy, pedalboard, fastapi, uvicorn,
pyperclip,                        # Safe clipboard access (replaces Win32 OpenClipboard)
nvidia-cublas-cu12, nvidia-cudnn-cu12  # CUDA DLLs — registered via os.add_dll_directory at startup
```

### Node (`package.json`)
```
react ^19.1.0, react-dom ^19.1.0, @tauri-apps/api ^2.5.0,
@tauri-apps/plugin-shell ^2.2.0, @tauri-apps/plugin-store ^2.4.2
Dev: @tauri-apps/cli ^2.5.0, vite ^6.3.0, typescript ~5.7.0, tailwindcss ^3.4.17
```

### Rust (`Cargo.toml`)
```
tauri 2 (tray-icon), tauri-plugin-shell 2, tauri-plugin-store 2,
arboard 3, serde 1, serde_json 1
```

---

## 15. Known Design Decisions & Trade-offs

1. **Greedy decoding** (`beam_size=1`): Sacrifices accuracy for speed. Acceptable for real-time dictation.
2. **`condition_on_previous_text=False`**: Prevents hallucination loops where Whisper repeats itself on silence remnants.
3. **VAD before Whisper** (not Whisper's built-in `vad_filter`): Silero runs on CPU, pre-filters silence to save GPU cycles. Whisper sees only speech.
4. **pyperclip + SendInput over WM_PASTE**: pyperclip avoids the Win32 clipboard lifecycle (OpenClipboard/CloseClipboard) that caused 0x0000000000000000 access violations in Electron/UWP/browser sandboxes. SendInput hardware-level Ctrl+V simulation works universally.
5. **`int8_float16` quantization**: Fits comfortably in 8 GB VRAM with room for other apps. Nearly no quality loss vs float32.
6. **Widget `focused(false)`**: Prevents the floating pill from stealing focus from the user's active application.
7. **Two independent hotkeys**: Hold (`hotkey_hold`) and hands-free (`hotkey_handsfree`) are separate binds. Eliminates the timing-based "< 350ms = toggle, ≥ 350ms = PTT" heuristic from the old single-hotkey design.
8. **Queue-based hotkey callbacks**: pynput callbacks are trivial `put_nowait` calls. All logic runs on a separate worker thread. Prevents Windows LowLevelHooksTimeout from killing the keyboard hook.
9. **Dual pynput listeners** (keyboard + mouse): Always both running regardless of hotkey type, so modifier tracking works for mouse-button hotkeys.
10. **48-hour history purge**: Prevents unbounded storage growth. Runs on frontend init only (not periodic).
11. **Volume RMS divisor = 3000**: Empirical normalization — maps typical mic levels to 0-1. Not calibrated per-device.
12. **Monitor polling at 500 ms**: Trade-off between responsiveness and CPU usage for multi-monitor widget repositioning.
13. **Model warm in VRAM at startup**: `load_model()` runs once in FastAPI `lifespan`. First dictation has zero cold-start penalty. GPU RAM consumed from app launch.
14. **Hard CUDA enforcement**: `_gpu_preflight()` + `Transcriber.load_model()` both call `ctranslate2.get_supported_compute_types("cuda")` and crash hard if unavailable. No silent CPU fallback that would cause 89% RAM usage and slow transcription.

---

## 16. Potential Risk Areas (For AI Analysis)

- **`_recording_mode` race**: `_do_start()`/`_do_stop()` acquire `_hotkey_lock`, but `_recording_mode` is read bare in `_handle_hold_press()` (the `if not models_ready or _recording_mode != "none"` guard) before the lock. This is a benign TOCTOU — worst case is a missed press, not corruption, since `_do_start()` re-checks under the lock.
- **`_active_modifiers` thread safety**: Mutated only by `_hotkey_worker` thread now (Queue pattern). Previously mutated directly on the pynput hook thread. The worker is the sole mutator, which eliminates data races for modifier state.
- **Queue backlog**: If the system is severely loaded, `_hotkey_queue` could grow. There is no `maxsize` limit. Rapid key presses during processing would queue up multiple start/stop events. The `_do_start`/`_do_stop` guards (mode != none checks) absorb duplicates safely.
- **PyAudio callback thread**: `_audio_callback` writes to `_frames` deque under lock, but `_recording` event and `_on_chunk` callback are not locked.
- **Silero VAD state**: `vad.reset()` called once at `_start_recording`. Since only one recording can be active at a time (`_hotkey_lock`), VAD state is never shared concurrently.
- **WebSocket set mutation**: `connected_ws` is a set mutated during `broadcast()` (stale removal) and during `websocket_endpoint()` (add/discard). Both run on the same event loop, so no true race, but `broadcast()` could be interrupted mid-iteration by a new connection.
- **pyperclip clipboard race**: `pyperclip.copy()` places text on clipboard before focus restore. If another application modifies the clipboard between `copy()` and `SendInput Ctrl+V` (~50-100ms window), the paste may produce wrong content. Very unlikely but not impossible.
- **SendInput Ctrl+V re-triggering**: If `hotkey_hold` or `hotkey_handsfree` includes Ctrl+V (unusual but possible), the injected Ctrl+V would create a loop. The `injected=True` filter in the queue worker discards injected events, preventing this.
- **`AttachThreadInput` leaks**: If the process crashes between attach and detach, the thread attachment persists until the threads exit. The `finally` block mitigates this in normal operation.
- **Settings write amplification**: Every `set()` call does a full JSON serialize + file write. High-frequency changes could cause I/O pressure (though in practice, settings change rarely).
- **Filler regex**: `\b(like,?)\b` will strip the word "like" everywhere, including legitimate uses ("I like this").
- **Stutter regex**: `\b(\w+)\s+\1\b` matches "that that" but also legitimate doubles ("go go", "bye bye").
- **No graceful shutdown signal**: If the Tauri process crashes, the Python sidecar may become an orphan (only killed on explicit "quit" tray action).
- **`_hotkey_worker_stop` Event timing**: `stop_hotkey_listener()` sets the stop event and joins with `timeout=2.0`. If the worker is blocked on a slow `_do_start` or `_do_stop`, it may not join within 2 s, leaving the thread running briefly after listeners are stopped.
- **Error notification dedup**: `lastError` triggers a new `Notification` every time it changes. If the same error repeats rapidly, it spawns multiple OS notifications.


---

## 17. Changelog (Recent Sessions)

### Session: Queue-based hotkey + SendInput paste (April 9, 2026)

#### ADDED
- **`_hotkey_queue: queue.Queue`** — global event queue; pynput callbacks enqueue tuples and return instantly
- **`_hotkey_worker()`** — daemon thread that drains the queue and runs all modifier tracking, key matching, and recording logic
- **`_hotkey_worker_thread`** and **`_hotkey_worker_stop`** — thread handle + Event for lifecycle management
- **`_worker_handle_key_press()`, `_worker_handle_key_release()`, `_worker_handle_mouse()`** — logic moved off the hook thread into worker
- **`pyperclip`** — replaces manual Win32 `OpenClipboard`/`SetClipboardData(CF_UNICODETEXT)`/`CloseClipboard` lifecycle
- **`_send_ctrl_v()`** in `typer.py` — `SendInput` 4-event Ctrl+V simulation (hardware-level, works in all apps)
- **`_make_key_input(vk, flags)`** — helper to build `INPUT` struct for `SendInput`
- **`KEYBDINPUT`, `INPUT` ctypes structs** in `typer.py`
- **`suppress=False`** explicit on both `pynput_kb.Listener` and `pynput_mouse.Listener`
- **Modifier cleanup failsafe** in worker `except` block: `_active_modifiers.clear()` when `_recording_mode == "none"`
- **`_gpu_preflight()`** in `server.py` — validates CTranslate2 CUDA at startup via `ctranslate2.get_supported_compute_types("cuda")`; crashes hard if CUDA unavailable
- **CUDA enforcement in `Transcriber.load_model()`** — same check, raises `RuntimeError` on failure
- **NVIDIA DLL path registration** — `server.py` prepends all `site-packages/nvidia/*/bin/` paths to both `os.environ["PATH"]` and `os.add_dll_directory()` before any CUDA import
- **Model warm-loading at startup** — `load_models()` runs in FastAPI `lifespan`, model stays in VRAM across all requests
- **Dual hotkeys**: `hotkey_hold` (hold to record) and `hotkey_handsfree` (toggle) — two independent configurable binds
- **`capture_target()`** now captures `(parent_hwnd, child_hwnd)` via `GetGUIThreadInfo` for caret-owning child element

#### CHANGED
- `_on_key_press`, `_on_key_release`, `_on_mouse_click` — now trivial `put_nowait` one-liners; all logic moved to worker
- `start_hotkey_listener()` — now starts worker thread; `stop_hotkey_listener()` joins it
- `_handle_hold_press()`, `_handle_hold_release()`, `_handle_handsfree_press()` — call `_do_start`/`_do_stop` directly; no longer spawn throwaway `threading.Thread` objects
- `type_text()` — flow is now `pyperclip.copy()` → `_restore_focus()` → `time.sleep(0.05)` → `_send_ctrl_v()`
- Settings keys renamed: `hotkey` → `hotkey_hold` + `hotkey_handsfree`

#### REMOVED
- `_set_clipboard_text()` — entire Win32 clipboard lifecycle (`OpenClipboard`, `GlobalAlloc`, `GMEM_MOVEABLE`, `CF_UNICODETEXT`, etc.)
- `WM_PASTE` (`0x0302`) — removed entirely; was causing `0x0000000000000000` access violations
- `WM_CHAR` (`0x0102`) — removed entirely
- `_pynput_fallback()` — character-by-character fallback via pynput type(); no longer needed
- `threading.Thread` spawns inside `_handle_hold_press/release/handsfree_press` — worker thread is already off the hook thread
- Timing-based PTT/hands-free heuristic (`< 350ms = toggle, ≥ 350ms = PTT`) — replaced by two distinct hotkeys
