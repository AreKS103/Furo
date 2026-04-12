import { useEffect, useRef, useState } from "react";
import { useFuro, type ServerState } from "../hooks/useFuro";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const IS_TAURI = "__TAURI_INTERNALS__" in window;

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

  useEffect(() => {
    document.documentElement.classList.add("dark");
    // Suppress scrollbar gutter so 100vw === window width, keeping pill centered.
    document.documentElement.style.overflow = "hidden";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    if (isActive) tauriShow();
  }, [isActive]);

  // Tauri window tracks the pill size: expand immediately (window ready before
  // CSS scale-up), collapse after 210 ms (CSS transition is 200 ms) so the
  // window never clips the animating pill. Hover is on the outer container
  // (stable hit zone) so resizing never affects hover detection.
  useEffect(() => {
    if (!IS_TAURI) return;
    if (showPopup) {
      // Popup overlays the pill at bottom-0, grows upward 100px
      invoke("widget_set_size", { width: 80, height: 100 }).catch(() => {});
      return;
    }
    if (expanded) {
      invoke("widget_set_size", { width: 80, height: 20 }).catch(() => {});
      return;
    }
    const timer = setTimeout(() => {
      invoke("widget_set_size", { width: 40, height: 10 }).catch(() => {});
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

  // ── Right-click: toggle last-transcription popup ──────────
  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setShowPopup((v) => !v);
  };

  // On macOS, mouse events on transparent windows can be unreliable.
  // Use a timeout to auto-collapse if mouse leaves and doesn't return.
  const hoverTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleEnter = () => {
    if (hoverTimeout.current) { clearTimeout(hoverTimeout.current); hoverTimeout.current = null; }
    setIsHovered(true);
  };
  const handleLeave = () => {
    // Small delay prevents flicker from macOS hit-test edge cases
    hoverTimeout.current = setTimeout(() => {
      setIsHovered(false);
      setShowPopup(false);
      if (isHoldingRef.current) {
        isHoldingRef.current = false;
        invoke("widget_hold_release").catch(() => {});
      }
    }, 80);
  };

  return (
    <div
      className="fixed inset-0 cursor-pointer"
      onMouseEnter={handleEnter}
      onMouseLeave={handleLeave}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
      onContextMenu={handleContextMenu}
    >
      {/* ── Popup: absolute at bottom-0, grows upward, covers the pill area */}
      <div
        className={`
          absolute bottom-0 left-1/2 -translate-x-1/2 w-[80px] overflow-hidden rounded-lg
          border border-white/30 shadow-lg shadow-black/30
          backdrop-blur-xl bg-black/80
          transition-[height,opacity] duration-200 ease-[cubic-bezier(0.4,0,0.2,1)]
          ${showPopup
            ? "h-[100px] opacity-100"
            : "h-0 opacity-0 pointer-events-none border-transparent shadow-none"
          }
        `}
      >
        <div className="h-full overflow-y-auto p-[6px] scrollbar-none">
          <p className="text-[8px] leading-[1.3] text-white/90 break-words select-text cursor-text">
            {displayText || "No transcription yet."}
          </p>
        </div>
      </div>

      {/* ── Pill: absolute at bottom-0, z-10 so it floats on top of the popup */}
      <div className="absolute inset-x-0 bottom-0 flex items-end justify-center z-10">
        <div
          className={`
            flex items-center justify-center rounded-full
            border shadow-lg shadow-black/30
            backdrop-blur-xl bg-black/80
            transition-[width,height,opacity,border-color] duration-200 ease-[cubic-bezier(0.4,0,0.2,1)]
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
