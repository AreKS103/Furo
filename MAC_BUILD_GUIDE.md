# Furo — AI Handoff Doc & macOS Build Guide

> **Last updated**: 2026-04-11  
> **Current version**: `0.2.10` (tauri.conf.json) / `0.2.0` (Cargo.toml — intentionally not synced)  
> **GitHub repo**: `https://github.com/AreKS103/Furo.git` (owner: AreKS103)

---

## Project Overview

Furo is a Tauri v2 + React 19 + TypeScript + Tailwind CSS desktop dictation app. The entire backend is native Rust — no Python. It captures audio via WASAPI/CoreAudio (cpal), filters with Silero VAD (ort/ONNX), transcribes via a `whisper-server` sidecar (whisper.cpp HTTP server), and injects text directly into the focused window.

### Module Map

| File | Purpose |
|------|---------|
| `src-tauri/src/lib.rs` | Tauri app entry: tray, windows, IPC commands, plugin setup |
| `src-tauri/src/pipeline.rs` | Central state machine: hotkey → record → VAD → transcribe → type |
| `src-tauri/src/audio.rs` | cpal microphone capture |
| `src-tauri/src/vad.rs` | Silero VAD v6 (ort 2.0) — 512-sample frames at 16 kHz |
| `src-tauri/src/transcriber.rs` | HTTP multipart POST to whisper-server `/v1/audio/transcriptions` |
| `src-tauri/src/sidecar.rs` | Spawn/manage `whisper-server` child process, drain stdout/stderr |
| `src-tauri/src/hotkey.rs` | Platform wrapper: `#[cfg]` dispatches to `hotkey_win.rs` / `hotkey_mac.rs` |
| `src-tauri/src/hotkey_mac.rs` | macOS global hotkey via CGEventTap (**created, compiles**) |
| `src-tauri/src/typer.rs` | Platform wrapper for text injection |
| `src-tauri/src/typer_mac.rs` | macOS text injection: clipboard + Cmd+V via CGEvent |
| `src-tauri/src/config.rs` | All tunables (ports, rates, URLs, timeouts) |
| `src-tauri/src/dsp.rs` | Highpass + high-shelf + compressor |
| `src-tauri/src/processor.rs` | Post-process transcription text (regex rules) |
| `src-tauri/src/settings.rs` | Persistent key-value store via tauri-plugin-store |
| `src-tauri/build.rs` | Copy sidecar binaries + DLLs to target dir; skip DLLs on non-Windows |
| `src/hooks/useFuro.ts` | Frontend IPC hook (invoke + listen on `furo://` events) |
| `src/components/Dashboard.tsx` | Main settings UI |
| `src/components/FloatingWidget.tsx` | Floating dictation pill overlay window |

### Key Config Values (`config.rs`)

```
WHISPER_SERVER_PORT = 8080
WHISPER_SERVER_URL  = "http://127.0.0.1:8080"
AUDIO_RATE          = 16_000 Hz
VAD_THRESHOLD       = 0.45
SIDECAR_STARTUP_TIMEOUT_SECS = 120
GGML_MODEL_FILENAME = "ggml-large-v3-turbo-q8_0.bin"
```

---

## CI/CD — GitHub Actions

Release workflow: `.github/workflows/release.yml`

- **Trigger**: push of `v*` tag (created by `npm run release:ci`)
- **Runners**: `windows-latest` (NSIS) + `macos-14` Apple Silicon (DMG)
- **Secrets required**: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- **macOS job**: clones whisper.cpp → cmake build (no Metal, `DWHISPER_BUILD_SERVER=ON`) → patches tauri.conf.json (removes DLL resources, fixes identifier) → `tauri build --target aarch64-apple-darwin --bundles dmg`
- **publish job**: downloads both artifacts, generates `latest.json`, creates GitHub Release

### Release Scripts (`package.json`)

| Script | What it does |
|--------|-------------|
| `npm run release:ci` | Bump patch, commit, tag, push → triggers CI |
| `npm run release:ci:minor` | Same but bumps minor |
| `npm run release:ci:major` | Same but bumps major |

---

## Platform Architecture

### Windows-only code

- `hotkey_win.rs` — `SetWindowsHookExW` keyboard hook, `SendInput`
- `typer_win.rs` — `SetClipboardData` + `SendInput` Ctrl+V
- `Cargo.toml`: `windows = "0.58"` under `[target.'cfg(target_os = "windows")'.dependencies]`
- `build.rs`: copies `.dll` files only when `target.contains("windows")`
- `lib.rs`: `.transparent(true).shadow(false)` on widget window only under `#[cfg(target_os = "windows")]`

### macOS-only code

- `hotkey_mac.rs` — CGEventTap global hotkey (Accessibility required)
- `typer_mac.rs` — NSWorkspace focus track + CGEvent Cmd+V (Accessibility required)
- `Cargo.toml`: `objc = "0.2"` under `[target.'cfg(target_os = "macos")'.dependencies]`

### `objc` crate API warning

`objc` v0.2.7 macro expansions emit many `unexpected_cfgs` warnings — **these are cosmetic, not errors**. Do not try to suppress them per-line; add `#![allow(unexpected_cfgs)]` at the top of any macOS file if they become disruptive.

`BOOL` in `objc::runtime` is `bool` on modern macOS Rust — **never compare with `!= 0`**, just use the value directly.

---

## Known Resolved Issues

| Issue | Fix location | Notes |
|-------|-------------|-------|
| `typer_mac.rs` BOOL comparison error | `typer_mac.rs:121` | Changed `ok != 0` → `ok` |
| `.transparent()` not found on macOS | `lib.rs` widget builder | Wrapped in `#[cfg(target_os = "windows")]` |
| DLLs copied during macOS build | `build.rs` DLL branch | Added `&& target.contains("windows")` guard |
| Whisper processing hangs forever | `sidecar.rs` | Dropped `_rx` was filling OS pipe buffer → deadlock. Fixed by spawning drain thread with `blocking_recv()` |
| NSIS installer "Error opening file: ggml-base.dll" | `src-tauri/windows/hooks.nsh` | Pre-install hook kills `whisper-server.exe` and `Furo.exe` |
| `.cargo/config.toml` breaking CI | `.gitignore` + `git rm --cached` | Local Windows path. File is gitignored; CI uses default target dir |

---

## Windows NSIS Pre-Install Hook

`src-tauri/windows/hooks.nsh` — kills running processes before NSIS copies files:

```nsh
!macro NSIS_HOOK_PREINSTALL
  ExecWait 'taskkill /F /IM "whisper-server.exe"'
  ExecWait 'taskkill /F /IM "Furo.exe"'
!macroend
```

Wired in via `tauri.conf.json`:
```json
"windows": { "nsis": { "installerHooks": "./windows/hooks.nsh" } }
```

---

## Auto-Updater

- Uses `tauri-plugin-updater`
- Endpoint in `tauri.conf.json`: `https://github.com/AreKS103/Furo/releases/latest/download/latest.json`
- `latest.json` is generated by the CI `publish` job and uploaded as a release asset
- Signing: minisign via `TAURI_SIGNING_PRIVATE_KEY` secret
- Bootstrap problem: versions older than 0.2.8 don't know the updater URL and cannot auto-update

---

## Cargo.toml Platform Dependencies (current state)

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation", "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse", "Win32_UI_Accessibility",
    "Win32_System_Threading", "Win32_System_LibraryLoader",
] }

[target.'cfg(target_os = "macos")'.dependencies]
objc = "0.2"
```

---

## macOS Build Prerequisites

| Tool | Install |
|------|---------|
| Xcode CLT | `xcode-select --install` |
| Homebrew | `https://brew.sh` |
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node.js 20+ | `brew install node` |
| CMake | `brew install cmake` |

Add aarch64 target (M1/M2 CI runner):
## Step-by-Step macOS Build

### 1. Clone and install

```bash
git clone https://github.com/AreKS103/Furo.git
cd Furo
npm ci
```

### 2. Build the whisper-server sidecar

The CI does this automatically. For local macOS builds:

```bash
git clone --depth 1 https://github.com/ggerganov/whisper.cpp.git /tmp/whisper.cpp
cd /tmp/whisper.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release -DWHISPER_BUILD_SERVER=ON -DWHISPER_COREML=OFF
cmake --build build --config Release -j$(sysctl -n hw.logicalcpu)

# Apple Silicon:
cp build/bin/whisper-server /path/to/Furo/src-tauri/binaries/whisper-server-aarch64-apple-darwin
chmod +x /path/to/Furo/src-tauri/binaries/whisper-server-aarch64-apple-darwin

# Intel Mac:
cp build/bin/whisper-server /path/to/Furo/src-tauri/binaries/whisper-server-x86_64-apple-darwin
```

The binary name **must** include the Rust target triple suffix.

### 3. Patch tauri.conf.json for local macOS build

The CI does this automatically via Python. For local builds, remove the DLL resources block from `tauri.conf.json`:

```json
"resources": {}
```

(The DLLs are Windows-only — they cause tauri-bundler errors on macOS.)

### 4. Build

```bash
# Apple Silicon:
npx tauri build --target aarch64-apple-darwin --bundles dmg

# Intel:
npx tauri build --target x86_64-apple-darwin --bundles dmg

# Dev mode:
npm run tauri dev
```

Output: `target/<triple>/release/bundle/dmg/Furo_<version>_<arch>.dmg`

### 5. macOS Permissions Required

Grant in **System Settings → Privacy & Security**:

1. **Microphone** — CoreAudio/cpal capture
2. **Accessibility** — text injection (`typer_mac.rs` CGEvent) and global hotkeys (`hotkey_mac.rs` CGEventTap)

---

## Feature Status on macOS

| Feature | Status |
|---------|--------|
| Audio (cpal/CoreAudio) | ✅ Works |
| VAD (ort/Silero) | ✅ Works |
| Transcription (whisper-server sidecar) | ✅ Works (CPU; Metal optional) |
| Text injection (typer_mac.rs) | ✅ Works (requires Accessibility) |
| Global hotkeys (hotkey_mac.rs) | ✅ Implemented (requires Accessibility) |
| Transparent widget window | ⚠️ No — `.transparent()` not available on macOS WebviewWindowBuilder; widget has opaque background |
| CUDA GPU acceleration | N/A — use Metal (not yet wired in whisper.cpp args) |
| System tray | ✅ Works |
| Auto-updater | ✅ Works (CI generates `.app.tar.gz` + sig) |
| NSIS pre-install hook | Windows only |

---

## SmartCode / Developer Notes

### Sidecar stdout drain (critical)

`sidecar.rs` spawns a drain thread that reads all `whisper-server` stdout/stderr via `blocking_recv()`. **Do not drop the receiver** — the OS pipe buffer is ~64 KB; if it fills, `whisper-server` blocks on its next write and all HTTP responses deadlock indefinitely.

### Widget window transparency

`lib.rs` uses `#[cfg(target_os = "windows")]` to apply `.transparent(true).shadow(false)` only on Windows. The macOS builder is built from a plain `WebviewWindowBuilder` ref — if you add more platform-specific methods, follow the same pattern.

### `whisper-server --inference-path` arg

The sidecar is started with `--inference-path /v1/audio/transcriptions`. This is whisper.cpp-specific and must match what `transcriber.rs` POSTs to. If you upgrade whisper.cpp, verify this endpoint still exists.

### Version numbers are split

- `package.json` + `tauri.conf.json`: in sync (bumped by `release:ci`)
- `Cargo.toml`: stays at `0.2.0` — Tauri reads the version from `tauri.conf.json`, not Cargo


