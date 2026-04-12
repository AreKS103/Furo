import { useEffect, useRef, useState } from "react";
import { useFuro, type ServerState } from "../hooks/useFuro";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/* ─── Inline icons (no icon library dependency) ──────────────────── */
function ClipboardIcon({ className = "" }: { className?: string }) {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none"
      stroke="currentColor" strokeWidth="1.5"
      strokeLinecap="round" strokeLinejoin="round"
      className={className}>
      <rect x="5" y="1.5" width="6" height="3" rx="1" />
      <path d="M4.5 3h-1A1.5 1.5 0 0 0 2 4.5v9A1.5 1.5 0 0 0 3.5 15h9a1.5 1.5 0 0 0 1.5-1.5v-9A1.5 1.5 0 0 0 12.5 3h-1" />
    </svg>
  );
}
function CheckIcon({ className = "" }: { className?: string }) {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none"
      stroke="currentColor" strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round"
      className={className}>
      <polyline points="3,8 6.5,12 13,4.5" />
    </svg>
  );
}

const IS_TAURI = "__TAURI_INTERNALS__" in window;
// On macOS, WKWebView never receives mousemove when another app is the key
// window, so DOM onMouseEnter/Leave are unreliable. We use a Rust polling
// thread instead (see lib.rs start_widget_hover_tracker) that pushes
// `widget-hover` Tauri events. On Windows the DOM events work fine.
const IS_MAC = IS_TAURI && navigator.platform.toUpperCase().includes("MAC");

const STORE_FILE = "dictation-history.json";
const STORE_KEY = "dictations";


async function tauriShow() {
  if (!IS_TAURI) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const win = getCurrentWindow();
  await win.show();
  // Do NOT call win.setFocus() — it steals focus from the user's active app.
}

/* ─── Audio Visualizer Bars ──────────────────────────────────────── */
const BAR_COUNT = 10;

function AudioVisualizer({
  volume,
  state,
}: {
  volume: number;
  state: ServerState;
}) {
  const [animTick, setAnimTick] = useState(0);
  const rafRef = useRef<number>(0);
  const smoothVolRef = useRef(0);
  const targetVolRef = useRef(0);

  // Push latest volume into ref so the RAF loop can read it synchronously.
  targetVolRef.current = volume;

  // 60fps RAF loop: lerps smoothVolRef toward the latest WS volume each frame,
  // giving fluid bar motion even though volume updates arrive every ~50ms.
  useEffect(() => {
    const tick = () => {
      // Lerp: 30% per frame ≈ 60ms time constant — responsive but jitter-free
      smoothVolRef.current +=
        (targetVolRef.current - smoothVolRef.current) * 0.3;
      setAnimTick((t) => t + 1);
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, []);

  const t = animTick;
  const v = smoothVolRef.current;
  const bars = Array.from({ length: BAR_COUNT }, (_, i) => {
    const centerDist =
      Math.abs(i - (BAR_COUNT - 1) / 2) / ((BAR_COUNT - 1) / 2);
    const weight = 1 - centerDist * 0.45;
    let scale: number;

    if (state === "recording") {
      // Volume-reactive bars — exponential curve keeps quiet sounds low while
      // loud speech can still reach full height. Avoids the "always at 100%" problem.
      const wave = Math.sin(t * 0.06 + i * 0.9) * 0.04;
      scale = 0.10 + Math.pow(v, 1.5) * weight * 1.5 + wave;
    } else if (state === "processing") {
      scale = 0.3 + Math.sin(t * 0.07 + i * 0.7) * 0.25;
    } else {
      scale = 0.15 + Math.sin(t * 0.025 + i * 0.4) * 0.05;
    }

    return Math.min(Math.max(scale, 0.05), 1.0);
  });

  return (
    <div className="flex items-center justify-center gap-[2px]">
      {bars.map((s, i) => (
        <div
          key={i}
          className="w-[2.5px] rounded-full bg-white/90"
          style={{
            height: "14px",
            transform: `scaleY(${s})`,
            opacity: 0.5 + s * 0.5,
          }}
        />
      ))}
    </div>
  );
}

/* ═══════════════════════════════════════════════════════════════════ */
export function FloatingWidget() {
  const { state, volume, lastText } = useFuro();
  const isActive = state === "recording" || state === "processing";
  const [isHovered, setIsHovered] = useState(false);
  const [showPopup, setShowPopup] = useState(false);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const expanded = isActive || isHovered || showPopup;
  const lastMonitorIdRef = useRef<string>("");
  const isHoldingRef = useRef(false);
  const [persistedText, setPersistedText] = useState("");

  // The text to display: prefer the current session's lastText, fall back to persisted history.
  const displayText = lastText || persistedText;

  // Load last transcription from persistent store on mount
  useEffect(() => {
    if (!IS_TAURI) return;
    (async () => {
      try {
        const { Store } = await import("@tauri-apps/plugin-store");
        const store = await Store.load(STORE_FILE);
        const saved = await store.get<{ id: string; text: string; timestamp: number }[]>(STORE_KEY);
        if (saved && saved.length > 0) {
          setPersistedText(saved[0].text);
        }
      } catch { /* store not available */ }
    })();
  }, []);

  // Keep persisted text in sync when new transcriptions arrive
  useEffect(() => {
    if (!IS_TAURI) return;
    const unsub = listen<{ text: string }>("furo://transcription", (event) => {
      if (event.payload.text) setPersistedText(event.payload.text);
    });
    return () => { unsub.then((fn) => fn()); };
  }, []);

  // Fade widget when any app goes fullscreen (video player, browser fullscreen, etc.)
  useEffect(() => {
    if (!IS_TAURI) return;
    const unsub = listen<boolean>("widget-fullscreen", (e) => {
      setIsFullscreen(e.payload);
      if (e.payload) setShowPopup(false);
    });
    return () => { unsub.then((fn) => fn()); };
  }, []);

  useEffect(() => {
    document.documentElement.classList.add("dark");
    // Suppress scrollbar gutter so 100vw === window width, keeping pill centered.
    document.documentElement.style.overflow = "hidden";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    if (isActive) tauriShow();
  }, [isActive]);

  // Tauri window sizing — keeps the native window exactly around the visible
  // content so the invisible hit-zone doesn't block clicks to other apps.
  //
  // Sizes (logical px):
  //   collapsed  → 40 × 10   (tiny idle pill)
  //   expanded   → 80 × 20   (hovered / recording pill)
  //   popup      → 80 × 62   (pill + 6px gap + 36px box)
  //
  // Smoothness rules:
  //   • EXPAND instantly — the window grows before CSS starts animating.
  //   • COLLAPSE after delay — wait for CSS animation to finish, then shrink.
  //   • Opening the popup pre-sizes in handleContextMenu (one frame ahead).
  const prevState = useRef({ expanded: false, showPopup: false });
  useEffect(() => {
    if (!IS_TAURI) return;
    const prev = prevState.current;
    prevState.current = { expanded, showPopup };

    // --- Determine target size ---
    let w: number, h: number;
    if (showPopup)       { w = 80; h = 62; }
    else if (expanded)   { w = 80; h = 20; }
    else                 { w = 40; h = 10; }

    // --- Growing? Resize immediately (no visible delay) ---
    const wasSmaller =
      (!prev.expanded && expanded) ||
      (!prev.showPopup && showPopup);
    if (wasSmaller) {
      invoke("widget_set_size", { width: w, height: h }).catch(() => {});
      return;
    }

    // --- Shrinking? Delay so CSS transition plays first ---
    const timer = setTimeout(() => {
      invoke("widget_set_size", { width: w, height: h }).catch(() => {});
    }, 210);
    return () => clearTimeout(timer);
  }, [expanded, showPopup]);

  // Multi-monitor: reposition widget to bottom-center of whichever screen
  // the mouse cursor is on. Polls every 500ms via physical coordinates.
  // Window size is always WIDGET_W × WIDGET_H — never changes.
  useEffect(() => {
    if (!IS_TAURI) return;

    const checkMonitor = async () => {
      try {
        const { availableMonitors, cursorPosition, getCurrentWindow } =
          await import("@tauri-apps/api/window");
        const { PhysicalPosition } = await import("@tauri-apps/api/dpi");

        const [cursor, monitors] = await Promise.all([
          cursorPosition(),
          availableMonitors(),
        ]);

        // Find which monitor contains the cursor (all in physical px)
        const monitor = monitors.find((m) => {
          const { x, y } = m.position;
          const { width, height } = m.size;
          return (
            cursor.x >= x &&
            cursor.x < x + width &&
            cursor.y >= y &&
            cursor.y < y + height
          );
        });

        if (!monitor) return;
        const id = `${monitor.position.x},${monitor.position.y}`;
        if (id === lastMonitorIdRef.current) return;
        lastMonitorIdRef.current = id;

        const scale = monitor.scaleFactor;
        const { x: mx, y: my } = monitor.position;
        const { width: mw, height: mh } = monitor.size;

        const win = getCurrentWindow();
        const curSize = await win.outerSize();
        const wW = curSize.width;
        const wH = curSize.height;
        const isMac = navigator.platform.toUpperCase().includes("MAC");
        const bottomOffset = isMac ? 100 : 60;

        await win.setPosition(
          new PhysicalPosition(
            mx + Math.round((mw - wW) / 2),
            my + mh - wH - Math.round(bottomOffset * scale),
          ),
        );
      } catch {
        // Tauri APIs unavailable in web dev mode — ignore
      }
    };

    const timer = setInterval(checkMonitor, 500);
    return () => clearInterval(timer);
  }, []);

  // ── Mouse-hold dictation: LEFT-click and hold to record ─────
  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return; // left click only
    isHoldingRef.current = true;
    invoke("widget_hold_start").catch(() => {});
  };
  const handleMouseUp = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    if (!isHoldingRef.current) return;
    isHoldingRef.current = false;
    invoke("widget_hold_release").catch(() => {});
  };

  // ── Right-click: toggle the box ──────────────────────────
  // Pre-size the Tauri window BEFORE the popup renders so there is never a
  // frame where the box is opacity-100 but the window is still 20px tall
  // (which would leave a transparent gap and leak the cursor to the app below).
  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!showPopup) {
      if (IS_TAURI) invoke("widget_set_size", { width: 80, height: 62 }).catch(() => {});
      requestAnimationFrame(() => setShowPopup(true));
    } else {
      setShowPopup(false);
    }
  };

  // ── Box click: copy last transcription to clipboard and re-paste ──
  const [copied, setCopied] = useState(false);
  const handleBoxClick = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!displayText) return;
    try {
      await navigator.clipboard.writeText(displayText);
    } catch { /* fallback: just paste via Tauri */ }
    if (IS_TAURI) {
      invoke("repaste_last", { text: displayText }).catch(() => {});
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  // On macOS, hover is driven by the Rust polling thread (widget-hover events)
  // because WKWebView stops receiving mousemove when another app is focused.
  // On Windows, the plain DOM events work fine.
  const hoverTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleEnter = () => {
    if (hoverTimeout.current) { clearTimeout(hoverTimeout.current); hoverTimeout.current = null; }
    setIsHovered(true);
  };
  const handleLeave = () => {
    // 200 ms absorbs the occasional false hover-off the Rust tracker can emit
    // while the window is mid-resize (e.g. right-click grow). 50 ms poll × 4
    // polls ≈ 200 ms window before we trust a sustained "not hovering" signal.
    hoverTimeout.current = setTimeout(() => {
      setIsHovered(false);
      setShowPopup(false);
      if (isHoldingRef.current) {
        isHoldingRef.current = false;
        invoke("widget_hold_release").catch(() => {});
      }
    }, 200);
  };

  // macOS: subscribe to `widget-hover` events pushed by the Rust cursor-
  // polling thread. This works regardless of which app is frontmost.
  useEffect(() => {
    if (!IS_MAC) return;
    let unlistenFn: (() => void) | undefined;
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen<boolean>("widget-hover", (e) => {
        if (e.payload) {
          handleEnter();
        } else {
          handleLeave();
        }
      })
    ).then((fn) => { unlistenFn = fn; });
    return () => { unlistenFn?.(); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      className={`fixed inset-0 cursor-default transition-opacity duration-500 ease-in-out ${
        isFullscreen ? "opacity-0 pointer-events-none" : "opacity-100"
      }`}
      onMouseEnter={IS_MAC ? undefined : handleEnter}
      onMouseLeave={IS_MAC ? undefined : handleLeave}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
      onContextMenu={handleContextMenu}
    >
      {/* ── The Box: icon-only pill floating 6px above the main pill.
           44×36px. Press effect via active:scale-95 + spring transition. */}
      <div
        className={`
          absolute left-1/2 -translate-x-1/2 w-[44px] h-[36px]
          rounded-2xl border bg-[#111]/[0.97]
          shadow-2xl shadow-black/70
          cursor-pointer select-none flex items-center justify-center
          transition-[opacity,transform] ease-[cubic-bezier(0.34,1.56,0.64,1)]
          active:scale-[0.88] active:duration-75
          ${showPopup
            ? "opacity-100 translate-y-0 pointer-events-auto border-white/[0.07] duration-150"
            : "opacity-0 translate-y-2 pointer-events-none border-transparent duration-150"
          }
        `}
        style={{ bottom: "26px" }}
        onClick={handleBoxClick}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div
          className={`transition-[color,transform] duration-200 ${
            copied
              ? "text-white/50 scale-110"
              : "text-white/35 scale-100"
          }`}
        >
          {copied ? <CheckIcon /> : <ClipboardIcon />}
        </div>
      </div>

      {/* ── Pill: absolute at bottom-0, z-10 so it floats on top of the popup */}
      <div className="absolute inset-x-0 bottom-0 flex items-end justify-center z-10">
        <div
          className={`
            flex items-center justify-center rounded-full
            border shadow-lg shadow-black/30
            backdrop-blur-xl bg-black/80
            transition-[width,height,opacity,border-color] duration-150 ease-out
            ${expanded
              ? "w-[80px] h-[20px] border-white/40"
              : "w-[40px] h-[10px] border-white/30 opacity-80"
            }
          `}
        >
          <div
            className="transition-opacity duration-200"
            style={{ opacity: expanded ? (isActive ? 1 : 0.5) : 0 }}
          >
            <AudioVisualizer volume={volume} state={state} />
          </div>
        </div>
      </div>
    </div>
  );
}
