# AI Architecture & Coding Mistakes Log
*Local project memory. Logs failed assumptions, broken code, and context errors.*
## Log Entries

### 2026-04-10
- **The Error:** Dashboard.tsx keeps getting duplicated content (file doubled in size) after `create_file` or `Remove-Item` + `create_file` operations.
- **The Mistake:** OneDrive sync can race with file writes, causing content to be appended instead of replaced. The `create_file` tool and `Set-Content` are both susceptible.
- **The Rule:** After any file write on OneDrive paths, ALWAYS verify line count and check for duplicate `import {` lines. Use `[System.IO.File]::WriteAllLines()` with explicit UTF-8 no-BOM encoding for atomic writes. If duplication occurs, truncate with `$content[0..N]` and rewrite.

### 2026-04-09
- **The Error:** `AttributeError: 'numpy.ndarray' object has no attribute 'dim'` in Silero VAD
- **The Mistake:** Assumed `silero_vad` OnnxWrapper accepts numpy arrays directly. It requires `torch.Tensor`.
- **The Rule:** Always pass `torch.from_numpy(array)` to Silero VAD, even in ONNX mode.

### 2026-04-09
- **The Error:** `RuntimeError: Library cublas64_12.dll is not found or cannot be loaded`
- **The Mistake:** Assumed CTranslate2 can find CUDA DLLs from pip-installed nvidia packages automatically on Windows.
- **The Rule:** On Windows, pip-installed NVIDIA packages put DLLs in `site-packages/nvidia/<pkg>/bin/`. Must call `os.add_dll_directory()` for each before importing CTranslate2. Also must be collected as binaries in PyInstaller spec.

