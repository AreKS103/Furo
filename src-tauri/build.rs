fn main() {
    // Copy sidecar binaries + DLLs to the target profile directory so the
    // Tauri shell plugin can resolve them at runtime. Necessary because
    // .cargo/config.toml redirects target-dir outside src-tauri/.
    //
    // tauri-plugin-shell's relative_command_path() looks for the binary
    // WITHOUT the target triple (e.g. "whisper-server.exe"), while our
    // source binaries are named WITH the triple (e.g.
    // "whisper-server-x86_64-pc-windows-msvc.exe") as required by
    // tauri-bundler for production builds. We therefore create an
    // additional triple-stripped copy of every sidecar .exe so dev mode
    // can find it.
    let target = std::env::var("TARGET").unwrap_or_default(); // e.g. "x86_64-pc-windows-msvc"
    let triple_suffix = format!("-{}", target);               // e.g. "-x86_64-pc-windows-msvc"

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src_binaries = std::path::Path::new(&manifest_dir).join("binaries");

    if src_binaries.exists() {
        // OUT_DIR is <target>/<profile>/build/<crate>-<hash>/out/
        // Walk up 3 ancestors to reach <target>/<profile>/
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let target_profile_dir = std::path::Path::new(&out_dir)
            .ancestors()
            .nth(3)
            .expect("could not resolve target profile dir");
        let dst_binaries = target_profile_dir.join("binaries");
        std::fs::create_dir_all(&dst_binaries).unwrap();

        for entry in std::fs::read_dir(&src_binaries).unwrap() {
            let entry = entry.unwrap();
            let src = entry.path();
            if !src.is_file() {
                continue;
            }

            // --- copy original file to binaries/ (keeps triple name for old externalBin path) ---
            let dst = dst_binaries.join(entry.file_name());
            let needs_copy = if dst.exists() {
                src.metadata().unwrap().modified().unwrap()
                    > dst.metadata().unwrap().modified().unwrap()
            } else {
                true
            };
            if needs_copy {
                println!("cargo::warning=copying sidecar: {}", entry.file_name().to_string_lossy());
                std::fs::copy(&src, &dst).unwrap();
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if let Some(stem) = name_str.strip_suffix(".exe") {
                if let Some(base) = stem.strip_suffix(&triple_suffix) {
                    // --- copy triple-stripped .exe to binaries/ for dev-mode resolution ---
                    // (tauri-plugin-shell sidecar() appends .exe without triple)
                    let short_name = format!("{}.exe", base);
                    let dst_short = dst_binaries.join(&short_name);
                    let needs_short = if dst_short.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_short.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_short {
                        println!("cargo::warning=copying sidecar (short name): {}", short_name);
                        std::fs::copy(&src, &dst_short).unwrap();
                    }

                    // --- ALSO copy triple-named exe to TARGET ROOT for tauri-bundler ---
                    // externalBin: ["whisper-server"] → bundler looks for
                    // <target_profile>/whisper-server-<triple>.exe (no path prefix)
                    let dst_root_triple = target_profile_dir.join(entry.file_name());
                    let needs_root_triple = if dst_root_triple.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_root_triple.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_root_triple {
                        println!("cargo::warning=copying sidecar to root: {}", entry.file_name().to_string_lossy());
                        std::fs::copy(&src, &dst_root_triple).unwrap();
                    }

                    // --- ALSO copy short-named exe to TARGET ROOT for dev sidecar resolution ---
                    // sidecar("whisper-server") → looks for <exe_dir>/whisper-server.exe
                    let dst_root_short = target_profile_dir.join(&short_name);
                    let needs_root_short = if dst_root_short.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_root_short.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_root_short {
                        println!("cargo::warning=copying sidecar short to root: {}", short_name);
                        std::fs::copy(&src, &dst_root_short).unwrap();
                    }
                }
            } else if name_str.ends_with(".dll") && target.contains("windows") {
                // --- copy DLLs to TARGET ROOT so whisper-server.exe can find them at runtime ---
                // In dev mode, the sidecar exe runs from <target_profile>/ so DLLs must be there.
                // Only on Windows — DLLs are irrelevant on macOS/Linux.
                let dst_root_dll = target_profile_dir.join(entry.file_name());
                let needs_dll = if dst_root_dll.exists() {
                    src.metadata().unwrap().modified().unwrap()
                        > dst_root_dll.metadata().unwrap().modified().unwrap()
                } else {
                    true
                };
                if needs_dll {
                    println!("cargo::warning=copying DLL to root: {}", entry.file_name().to_string_lossy());
                    std::fs::copy(&src, &dst_root_dll).unwrap();
                }
            } else if !name_str.contains('.') {
                // --- macOS: extensionless sidecar binaries (e.g. whisper-server-aarch64-apple-darwin) ---
                if let Some(base) = name_str.strip_suffix(&triple_suffix) {
                    // Copy triple-stripped binary for dev-mode resolution
                    let dst_short = dst_binaries.join(base);
                    let needs_short = if dst_short.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_short.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_short {
                        println!("cargo::warning=copying sidecar (short name): {}", base);
                        std::fs::copy(&src, &dst_short).unwrap();
                    }

                    // Copy triple-named binary to target root for bundler
                    let dst_root_triple = target_profile_dir.join(entry.file_name());
                    let needs_root = if dst_root_triple.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_root_triple.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_root {
                        println!("cargo::warning=copying sidecar to root: {}", name_str);
                        std::fs::copy(&src, &dst_root_triple).unwrap();
                    }

                    // Copy short-named binary to target root for dev sidecar resolution
                    let dst_root_short = target_profile_dir.join(base);
                    let needs_root_short = if dst_root_short.exists() {
                        src.metadata().unwrap().modified().unwrap()
                            > dst_root_short.metadata().unwrap().modified().unwrap()
                    } else {
                        true
                    };
                    if needs_root_short {
                        println!("cargo::warning=copying sidecar short to root: {}", base);
                        std::fs::copy(&src, &dst_root_short).unwrap();
                    }
                }
            }
        }

        println!("cargo:rerun-if-changed=binaries");
    }

    tauri_build::build();
    // Ensure winmm.lib is linked for PlaySoundA (widget activation audio).
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-lib=winmm");
}
