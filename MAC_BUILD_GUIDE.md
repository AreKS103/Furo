# Building Furo on macOS (Intel)

> **Status**: The macOS port is partially complete. Audio, transcription, VAD, and text injection (`typer_mac.rs`) work. **`hotkey_mac.rs` does not exist yet** — you must create it before building. The steps below assume you will stub or implement it first.

---

## Prerequisites

| Tool | Install Command |
|------|----------------|
| Xcode Command Line Tools | `xcode-select --install` |
| Homebrew | `/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"` |
| Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node.js 20+ | `brew install node` |
| CMake | `brew install cmake` |

After installing Rust, restart your terminal and confirm:

```bash
rustc --version
cargo --version
node --version
npm --version
cmake --version
```

---

## Step 1 — Clone the Repo

```bash
git clone https://github.com/AreKS103/Furo.git
cd Furo
```

---

## Step 2 — Install Node Dependencies

```bash
npm ci
```

---

## Step 3 — Cargo.toml Changes (Required)

The `windows` crate in `src-tauri/Cargo.toml` must be moved behind a target gate so it doesn't compile on macOS. Open `src-tauri/Cargo.toml` and replace:

```toml
# Win32 API: hooks, focus, SendInput, threading
windows = { version = "0.58", features = [...] }
```

With a target-specific section at the bottom:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_Accessibility",
    "Win32_System_Threading",
    "Win32_System_LibraryLoader",
] }

[target.'cfg(target_os = "macos")'.dependencies]
objc = "0.2"
```

And remove the original `windows = ...` line from `[dependencies]`.

---

## Step 4 — Create `hotkey_mac.rs` (Stub)

Create `src-tauri/src/hotkey_mac.rs` with at minimum a stub that satisfies the interface:

```rust
//! macOS global hotkey — CGEventTap implementation (stub).

use super::{HotkeyCallbacks, HotkeyEvent};

/// Start the macOS event tap for global hotkeys.
pub(crate) fn start_hotkey_thread(
    _hotkey_vks: Vec<u32>,
    _mouse_buttons: Vec<u32>,
    _callbacks: HotkeyCallbacks,
) {
    log::warn!("macOS hotkey not yet implemented");
}

/// Stop the event tap.
pub(crate) fn stop_hotkey_thread() {}
```

A full implementation would use `CGEventTapCreate` to intercept key events globally (requires Accessibility permission).

---

## Step 5 — Build the whisper-server Sidecar

Furo uses a `whisper-server` sidecar binary built from [whisper.cpp](https://github.com/ggerganov/whisper.cpp). On Intel Mac, build with Accelerate (or Metal for GPU):

```bash
git clone https://github.com/ggerganov/whisper.cpp.git
cd whisper.cpp
cmake -B build -DGGML_METAL=ON
cmake --build build --config Release -j$(sysctl -n hw.ncpu)
```

Copy the server binary into the Furo sidecar location:

```bash
# The triple for Intel Mac:
cp build/bin/whisper-server \
   /path/to/Furo/src-tauri/binaries/whisper-server-x86_64-apple-darwin
```

The binary name **must** include the Rust target triple suffix.

---

## Step 6 — Remove Windows-only Resources

In `src-tauri/tauri.conf.json`, the `bundle.resources` section lists `.dll` files. On macOS these are not needed. Either:

- Remove or comment out the `resources` block, **or**
- Use platform-conditional bundling (Tauri doesn't natively support this — simplest is to just remove the DLLs before building on Mac and clear the `resources` object)

---

## Step 7 — Build

### Dev mode (hot reload):

```bash
npm run tauri dev
```

### Release build:

```bash
npm run tauri build
```

Output: `src-tauri/target/release/bundle/dmg/Furo_<version>_x64.dmg`

---

## Step 8 — macOS Permissions

Furo needs two macOS permissions (prompted on first use):

1. **Microphone access** — for audio capture via CoreAudio/cpal
2. **Accessibility access** — for text injection (`typer_mac.rs` uses CGEvent) and global hotkeys (once `hotkey_mac.rs` is implemented)

Grant both in **System Settings → Privacy & Security**.

---

## Known Limitations on macOS

| Feature | Status |
|---------|--------|
| Audio (cpal/CoreAudio) | Works |
| VAD (ort/Silero) | Works |
| Transcription (whisper-server sidecar) | Works (Metal GPU on Intel) |
| Text injection (typer_mac.rs) | Works (requires Accessibility) |
| Global hotkeys (hotkey_mac.rs) | **Not implemented** — needs CGEventTap |
| CUDA GPU acceleration | N/A — Mac uses Metal instead |
| System tray | Works (Tauri built-in) |
| Auto-updater | Works (needs `.tar.gz` artifacts for macOS) |
