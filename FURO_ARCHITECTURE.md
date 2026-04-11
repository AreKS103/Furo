# Project Furo — Architecture & Logic Reference

> **For AI assistants**: This document replaces all prior architecture docs. The Python/FastAPI/WebSocket architecture described in old versions no longer exists. The backend is 100% native Rust.  
> **Date**: 2026-04-11 | **Version**: 0.2.10 | **Repo**: https://github.com/AreKS103/Furo  
> **Stack**: Tauri v2 (Rust) + React 19 + TypeScript + Tailwind CSS 3  
> **Hardware**: Windows 10/11 primary, macOS M1 secondary (CI only), RTX 2070 8GB

---

## 1. What Furo Does

A free, local, offline voice-to-text dictation tool. User presses a global hotkey in **any application**, speaks, and the transcribed text is typed into whatever window was focused. No cloud, no telemetry, no Python.

---

## 2. Dev Commands

All npm commands run from the workspace root (`./Furo`), not from `src-tauri`.

| Command | What it does |
|---------|-------------|
| `npm run dev` | Vite dev server only |
| `npm run tauri dev` | Full Tauri dev (Vite + Rust hot-reload) |
| `npm run tauri build` | Local Windows release build |
| `npm run release:ci` | Bump patch, commit, tag, push → triggers GitHub Actions CI |
| `npm run release:ci:minor` | Same, minor bump |
| `npm run release:ci:major` | Same, major bump |

---

## 3. High-Level Architecture

No WebSocket. No REST API. No Python. All frontendâ†”backend communication is Tauri IPC (invoke + emit).

```
Tauri v2 Shell
  â”œâ”€â”€ Main Window (React/TS — Settings + History)
  â”œâ”€â”€ Floating Widget (frameless, transparent pill, always-on-top)
  â””â”€â”€ System Tray (3 recent transcriptions + quit)
        â†• Tauri IPC
Native Rust Pipeline
  Hotkey → Audio → VAD → DSP → HTTP POST → Post-process → Typer
        â†• HTTP (localhost:8080)
whisper-server sidecar (whisper.cpp, /v1/audio/transcriptions)
```

---

## 4. Module Map

| Rust file | Purpose |
|-----------|---------|
| `lib.rs` | App entry: create windows, tray, register IPC commands, start pipeline |
| `pipeline.rs` | Central state machine: all recording lifecycle, event emission, thread coordination |
| `audio.rs` | cpal microphone capture, 16 kHz mono i16, streaming chunks to callbacks |
| `vad.rs` | Silero VAD v6 via ort (ONNX), 512-sample frames, stateful/recurrent |
| `transcriber.rs` | HTTP multipart POST to whisper-server; WAV encode; filler/stutter strip |
| `sidecar.rs` | Spawn whisper-server child; drain stdout/stderr thread; health-poll until ready |
| `hotkey.rs` | Platform wrapper + shared types (HotkeyEvent, combo parsing, worker thread) |
| `hotkey_win.rs` | Windows: SetWindowsHookExW low-level keyboard + raw mouse hook |
| `hotkey_mac.rs` | macOS: CGEventTap (requires Accessibility permission) |
| `typer.rs` | Platform wrapper + CapturedTarget struct |
| `typer_win.rs` | Windows: arboard clipboard + SendInput Ctrl+V |
| `typer_mac.rs` | macOS: NSWorkspace focus track + CGEvent Cmd+V |
| `dsp.rs` | Highpass 300 Hz → high-shelf 2500 Hz +5 dB → compressor -20 dB 3:1 |
| `processor.rs` | Voiced punctuation dict, "scratch that" commands, filler/stutter regex |
| `config.rs` | All tunables (ports, rates, thresholds, URLs, filenames) |
| `settings.rs` | %APPDATA%/Furo/settings.json, thread-safe read/write via parking_lot |
| `build.rs` | Copy sidecar binaries to target dir; skip .dll on non-Windows |

| Frontend file | Purpose |
|---------------|---------|
| `src/main.tsx` | Entry: routes to Dashboard or FloatingWidget by `?window=widget` param |
| `src/App.tsx` | Theme state, dark-mode class toggle, renders Dashboard |
| `src/components/Dashboard.tsx` | Settings tab (mic, hotkeys) + History tab (Notion cards) + status bar |
| `src/components/FloatingWidget.tsx` | Transparent pill, audio visualizer, multi-monitor repositioning |
| `src/hooks/useFuro.ts` | All Tauri IPC: invoke + listen on `furo://` events |
| `src/hooks/useHistory.ts` | tauri-plugin-store history, 48h purge, tray sync |

---

## 5. Pipeline State Machine (`pipeline.rs`)

`FuroPipeline` owns all mutable state behind `parking_lot::Mutex`.

### Startup (background thread, runs once at app launch)
1. Load Silero VAD ONNX model (CPU) → `vad` field
2. Ensure Whisper GGML model exists (download ~900 MB if missing) → path
3. Spawn `whisper-server` sidecar → poll `/health` until 200 OK (up to 120s)
4. Create Transcriber HTTP client → warmup call (0.1s silence, hides CUDA JIT latency ~90s)
5. Emit `furo://status ready` → start hotkey listener

### Dictation lifecycle
1. **Hotkey press** → `on_hold_press()` or `on_handsfree_press()`
2. Capture target window (`typer::capture_target()`) → stored in `captured_target`
3. Reset VAD state; clear speech buffer; play activation sound (cpal output, separate thread)
4. Start audio recording (cpal input stream)
5. Per 512-sample chunk: VAD classifies → speech chunks push to `speech_buffer`; RMS → `furo://volume`
6. **Hotkey release** → stop recording → spawn `furo-process` thread
7. Drain speech buffer → concat → DSP condition → WAV encode → HTTP POST to whisper-server
8. Response text → `processor.rs` → emit `furo://transcription`
9. Restore focus → `typer::type_text()` injects text into captured window
10. Emit `furo://status ready`

### Recording modes
- `Hold` — active while key held, stops on release
- `Handsfree` — toggles on second press
- `None` — idle

---

## 6. Hotkey System

### Architecture
Platform hook → `crossbeam_channel` → **worker thread** processes. Hook thread ONLY enqueues. This prevents Windows `LowLevelHooksTimeout` (~200ms) from killing the hook.

### Combo format
`"ctrl+win+space"`, `"alt+f5"`, `"mouse4"`, `"ctrl+mouse5"`, `"vk166"` (raw VK code for macro keys)

### Parsing
Split on `+`; last token = trigger, rest = required modifiers. Trigger: named key / single char / `vk<int>` / `mouse3/4/5`.

### Worker thread
- Tracks `active_modifiers: HashSet<String>` — worker is sole mutator (no races)
- On press: if trigger matches AND required modifiers âŠ† active → fire callback
- On release: if Hold trigger released → `on_hold_release`; if required modifier released during Hold → also release
- Injected keystrokes (from Furo's own paste) filtered via `injected=true` flag

### Windows (`hotkey_win.rs`)
`SetWindowsHookExW(WH_KEYBOARD_LL)` + raw mouse hook. `WIN_IS_COMBO_MODIFIER` static suppresses Win→Start and Win+Space during hotkey use. `REBIND_MODE_ACTIVE` static puts worker into capture mode for rebinding.

### macOS (`hotkey_mac.rs`)
`CGEventTapCreate` at `kCGSessionEventTap` level. Dedicated thread + CFRunLoop. Requires Accessibility permission.

---

## 7. Text Injection

### What gets captured
`capture_target()` saves the foreground window/process **at hotkey press time**, before recording starts. Text injects to the right place even if user clicks elsewhere during the 2-5 second transcription wait.

### Windows (`typer_win.rs`)
Target = HWND. Injection: `AttachThreadInput` to foreground + target threads → `SetForegroundWindow` → arboard clipboard set → `SendInput` 4-event Ctrl+V (VK_CONTROL↓ VK_V↓ VK_V← VK_CONTROL←) → 50ms settle delay.

### macOS (`typer_mac.rs`)
Target = pid_t. Injection: `NSRunningApplication::activateIgnoringOtherApps` → arboard clipboard set → CGEvent Cmd+V. `BOOL` from `objc::runtime` is `bool` on modern macOS — never compare with `!= 0`.

### Why clipboard + paste (not per-char SendInput)
Works in every app: Electron, browser sandboxes, UWP, terminal emulators. Per-character `SendInput`/`WM_CHAR` fails in IME-aware and sandboxed apps.

---

## 8. Whisper Sidecar (`sidecar.rs`)

Spawns `whisper-server` (whisper.cpp compiled with `-DWHISPER_BUILD_SERVER=ON`) as a Tauri sidecar child process.

Args: `--model <path> --host 127.0.0.1 --port 8080 --flash-attn --inference-path /v1/audio/transcriptions`

**Critical**: stdout/stderr receiver MUST be drained in a background thread via `blocking_recv()`. Dropping it fills the OS pipe buffer (~64 KB) → `whisper-server` blocks on stdout write → all HTTP responses deadlock.

Binary location: production = `<exe_dir>/whisper-server.exe` (NSIS strips `binaries/` prefix). Dev = `<exe_dir>/binaries/whisper-server.exe` (build.rs copies it).

Default model: `ggml-large-v3-turbo-q8_0.bin` (~900 MB) stored in `%APPDATA%/Furo/models/`. Transcriber scans system for existing GGML models first before downloading.

Windows DLLs (`ggml.dll`, `ggml-base.dll`, `ggml-cpu.dll`, `whisper.dll`, `libomp140.x86_64.dll`) bundled in `src-tauri/binaries/` and listed in `tauri.conf.json bundle.resources`. CI clears these for macOS builds.

---

## 9. VAD Pipeline (`vad.rs`)

Silero VAD v6, ONNX (~2 MB), CPU-only (GPU reserved for Whisper). Inputs: `input[1,576]` (512 samples + 64-sample context prepend), `state[2,1,128]` (GRU hidden), `sr[1]`. Threshold: 0.45 (configurable).

Stateful — `reset()` between recordings. Runs **streaming during recording** so Whisper only ever sees speech-only audio. Pre-roll: last 3 silent chunks prepended on speech onset (captures leading consonants). Hangover: 15 chunks (480ms) after speech drops below threshold.

---

## 10. DSP + Post-processing

DSP (`dsp.rs`, runs before Whisper): Highpass 300 Hz → high-shelf 2500 Hz +5 dB → compressor -20 dB 3:1.

Post-processing (`processor.rs`, runs after Whisper, deterministic, no LLM):
1. Symbol dict: voiced punctuation → symbols ("period" → ".", "open paren" → "(", etc.)
2. Voice commands: "scratch that" → delete last word, "I mean X" → replace last word
3. Filler strip: uh, um, er, ah, hm, hmm, like
4. Stutter dedup: repeated-word pairs
5. Space collapse

---

## 11. Frontend IPC

Events from Rust to frontend (frontend listens via `useFuro.ts`):

| Event | Payload | Meaning |
|-------|---------|---------|
| `furo://status` | `{state, message}` | State + display string |
| `furo://volume` | `{level: f64}` | Normalised mic level 0â€“1, ~50ms |
| `furo://transcription` | `{text}` | Final text (empty = no speech) |
| `furo://error` | `{message}` | Non-fatal error |
| `furo://settings` | `{data: HashMap}` | Full settings on load + after changes |
| `furo://rebind-capture` | combo string | Hotkey rebind UI |
| `furo://model-download-progress` | `{progress, message}` | Download 0â€“100 |

Status states: `loading` → `ready` → `recording` → `processing` → `ready`

### FloatingWidget
Frameless, transparent (Windows-only — `.transparent()` not available on macOS WebviewWindowBuilder), always-on-top, `skip_taskbar`, `focused(false)`. Position: bottom-center of cursor's monitor, polls every 500ms. 10-bar visualizer via RAF + lerp.

### Dashboard
Settings tab: mic dropdown (filtered by `MIC_EXCLUDE_KEYWORDS`), hotkey rebind (keyboard + mouse + macro vk codes). History tab: Notion-style cards, copy per entry, clear all. Status bar: color-coded badge.

---

## 12. Settings

File: `%APPDATA%/Furo/settings.json`

| Key | Default | Notes |
|-----|---------|-------|
| `microphone` | `""` | Empty = system default |
| `hotkey_hold` | `"ctrl+space"` | Hold-to-record |
| `hotkey_handsfree` | `"ctrl+shift+space"` | Toggle record |
| `language` | `"en"` | Whisper language; empty = auto-detect |
| `vad_threshold` | `"0.45"` | 0.0â€“1.0 |
| `sound_enabled` | `"true"` | Activation sound |
| `sound_volume` | `"0.05"` | 0.0â€“1.0 linear |
| `theme` | `"dark"` | UI theme |

Changing hotkey keys restarts the hotkey listener.

---

## 13. System Tray + Windowing

- 3 most-recent transcriptions as quick-copy menu items
- `open` → show main window; `quit` → pipeline shutdown + sidecar kill + exit
- Close button → `prevent_close()` + `window.hide()` (close-to-tray, not quit)
- Frontend emits Tauri event `update-tray` after each transcription → Rust updates `RecentTexts` → rebuilds tray menu

---

## 14. CI/CD (GitHub Actions)

File: `.github/workflows/release.yml`. Trigger: push of `v*` tag via `npm run release:ci` (run from `./Furo`).

**build-windows** (`windows-latest`): `tauri build --bundles nsis` → NSIS `.exe`, `.nsis.zip`, `.nsis.zip.sig`

**build-macos** (`macos-14` M1): clones whisper.cpp → cmake (`-DWHISPER_BUILD_SERVER=ON`, no Metal) → copies `whisper-server-aarch64-apple-darwin` to `src-tauri/binaries/` → Python patches `tauri.conf.json` (clears DLL resources) → `tauri build --target aarch64-apple-darwin --bundles dmg` → `.dmg`, `.app.tar.gz`, `.app.tar.gz.sig`

**publish** (`ubuntu-latest`): merges artifacts, generates `latest.json` (signed URLs for both platforms), creates GitHub Release.

Secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (GitHub repo → Settings → Secrets).

Auto-updater endpoint: `https://github.com/AreKS103/Furo/releases/latest/download/latest.json`. Works for 0.2.8+.

---

## 15. Windows NSIS Installer

Pre-install hook (`src-tauri/windows/hooks.nsh`) kills `whisper-server.exe` and `Furo.exe` before copying files. Without this, NSIS fails with "Error opening file for writing: ggml-base.dll" because the running app holds DLL file locks. Wired via `tauri.conf.json bundle.windows.nsis.installerHooks`.

---

## 16. Platform Differences

| Concern | Windows | macOS |
|---------|---------|-------|
| Hotkey hook | `SetWindowsHookExW` + raw mouse | `CGEventTap` (Accessibility perm required) |
| Text inject | `AttachThreadInput` + `SendInput` Ctrl+V | `activateIgnoringOtherApps` + CGEvent Cmd+V |
| Target capture | HWND | pid_t |
| Widget transparency | `.transparent(true).shadow(false)` | Not available — opaque background |
| Sidecar binary | `whisper-server.exe` + `.dll` files alongside | `whisper-server` (no extension, no DLLs) |
| DLL resources | Listed in `tauri.conf.json` | Cleared by CI Python patch step |
| Cargo target | `x86_64-pc-windows-msvc` | `aarch64-apple-darwin` (CI) or `x86_64-apple-darwin` (old Intel) |
| GPU | ggml-cuda.dll auto-loaded if present | Metal (not yet wired in sidecar args) |

### Platform-gated Cargo deps
- Windows: `windows = "0.58"` (Win32 APIs) under `[target.'cfg(target_os = "windows")'.dependencies]`
- macOS: `objc = "0.2"` under `[target.'cfg(target_os = "macos")'.dependencies]`

### lib.rs widget transparency pattern
`.transparent(true).shadow(false)` is wrapped in `#[cfg(target_os = "windows")]` because `WebviewWindowBuilder` on macOS doesn't expose these methods.

---

## 17. Version Numbers

- `package.json` + `tauri.conf.json`: always in sync, bumped by `release:ci` scripts
- `src-tauri/Cargo.toml`: stays at `0.2.0` — Tauri reads version from `tauri.conf.json`, not Cargo
- `.cargo/config.toml`: machine-local (redirects target dir out of OneDrive to `%LOCALAPPDATA%/furo-target`), gitignored; CI uses default target dir

---

## 18. Design Decisions

- **No silent CPU fallback**: whisper-server failure shows error, not CPU transcription.
- **VAD before Whisper**: Silero on CPU pre-filters so Whisper never sees silence (no hallucinations, saves GPU cycles).
- **Clipboard + paste**: Works in Electron, browsers, UWP. Per-char `SendInput` fails in IME-aware/sandboxed apps.
- **Queue-based hotkey callbacks**: Hook thread enqueues only. Worker thread processes. Prevents `LowLevelHooksTimeout`.
- **`focused(false)` on widget**: Never steals focus from user's app.
- **arboard for clipboard**: Cross-platform, handles Win32 locking. Replaces OpenClipboard/GlobalAlloc/SetClipboardData chain that caused access violations in sandboxed processes.
- **`condition_on_previous_text` effectively false** (fresh HTTP POST each recording): Prevents Whisper hallucination loops on silence remnants.

---

## 19. Risk Areas / Gotchas

- **Sidecar stdout drain (critical)**: Must drain `rx` via `blocking_recv()` in background thread. Dropping `_rx` fills OS pipe buffer → whisper-server deadlocks → all HTTP hang forever.
- **`_recording_mode` TOCTOU**: Read bare before `_do_start` acquires lock — benign (missed press at worst, re-checked under lock).
- **Clipboard race**: ~50-100ms window between `set_text()` and `SendInput` where another app could clobber clipboard.
- **Injected Ctrl+V re-trigger**: Filtered via `injected=true` flag on Windows; safe.
- **`AttachThreadInput` leak**: If process crashes mid-attach, attachment persists until threads exit. Normal operation uses `finally`-style pattern.
- **Model scan false positives**: Accepts any `.bin` ≥ 10 MB named `ggml-*` or `whisper-*`. Could pick non-Whisper model with matching name.
- **Filler regex**: Strips "like" everywhere (including legitimate uses). Stutter dedup strips "bye bye", "go go", etc.
- **Settings write amplification**: Every `set()` serializes full JSON. Fine for rare changes.
- **objc warnings**: `objc` v0.2.7 macro expansions emit many `unexpected_cfgs` — cosmetic only, not errors.
