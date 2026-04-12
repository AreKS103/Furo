import { useEffect, useRef, useState } from "react";
import { useFuro, type ServerState } from "../hooks/useFuro";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/* ─── Inline icons ───────────────────────────────────────────────── */
function ClipboardIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none"
      stroke="currentColor" strokeWidth="1.5"
      strokeLinecap="round" strokeLinejoin="round">
      <rect x="5" y="1.5" width="6" height="3" rx="1" />
      <path d="M4.5 3h-1A1.5 1.5 0 0 0 2 4.5v9A1.5 1.5 0 0 0 3.5 15h9a1.5 1.5 0 0 0 1.5-1.5v-9A1.5 1.5 0 0 0 12.5 3h-1" />
    </svg>
  );
}
function CheckIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none"
      stroke="currentColor" strokeWidth="1.8"
      strokeLinecap="round" strokeLinejoin="round">
      <polyline points="3,8 6.5,12 13,4.5" />
    </svg>
  );
}

const IS_TAURI = "__TAURI_INTERNALS__" in window;
const IS_MAC = IS_TAURI && navigator.platform.toUpperCase().includes("MAC");
const STORE_FILE = "dictation-history.json";
const STORE_KEY = "dictations";

// Smooth ease curves — no bounce/overshoot
const EASE_OUT = "cubic-bezier(0.25, 0.1, 0.25, 1)";
const DURATION = "150ms";

async function tauriShow() {
  if (!IS_TAURI) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().show();
}

/* ─── Audio Visualizer Bars ──────────────────────────────────────── */
const BAR_COUNT = 10;

function AudioVisualizer({ volume, state }: { volume: number; state: ServerState }) {
  const [tick, setTick] = useState(0);
  const raf = useRef(0);
  const smoothVol = useRef(0);
  const targetVol = useRef(0);
  targetVol.current = volume;

  useEffect(() => {
    const loop = () => {
      smoothVol.current += (targetVol.current - smoothVol.current) * 0.3;
      setTick((t) => t + 1);
      raf.current = requestAnimationFrame(loop);
    };
    raf.current = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf.current);
  }, []);

  const v = smoothVol.current;
  const bars = Array.from({ length: BAR_COUNT }, (_, i) => {
    const centerDist = Math.abs(i - (BAR_COUNT - 1) / 2) / ((BAR_COUNT - 1) / 2);
    const weight = 1 - centerDist * 0.45;
    let scale: number;
    if (state === "recording") {
      scale = 0.10 + Math.pow(v, 1.5) * weight * 1.5 + Math.sin(tick * 0.06 + i * 0.9) * 0.04;
    } else if (state === "processing") {
      scale = 0.3 + Math.sin(tick * 0.07 + i * 0.7) * 0.25;
    } else {
      scale = 0.15 + Math.sin(tick * 0.025 + i * 0.4) * 0.05;
    }
    return Math.min(Math.max(scale, 0.05), 1.0);
  });

  return (
    <div className="flex items-center justify-center gap-[2px]">
      {bars.map((s, i) => (
        <div
          key={i}
          className="w-[2.5px] rounded-full bg-white/90"
          style={{ height: "14px", transform: `scaleY(${s})`, opacity: 0.5 + s * 0.5 }}
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
  const [copied, setCopied] = useState(false);
  const [persistedText, setPersistedText] = useState("");
  const isHoldingRef = useRef(false);
  const lastMonitorIdRef = useRef("");
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const expanded = isActive || isHovered || showPopup;
  const displayText = lastText || persistedText;

  // ── Setup: dark mode, overflow visible, load history ──────────
  useEffect(() => {
    document.documentElement.classList.add("dark");
    // Allow content to overflow the viewport (Tauri window clips at bounds,
    // but CSS overflow:visible prevents the webview from double-clipping).
    document.documentElement.style.overflow = "visible";
    document.body.style.overflow = "visible";
  }, []);

  useEffect(() => {
    if (!IS_TAURI) return;
    (async () => {
      try {
        const { Store } = await import("@tauri-apps/plugin-store");
        const store = await Store.load(STORE_FILE);
        const saved = await store.get<{ id: string; text: string; timestamp: number }[]>(STORE_KEY);
        if (saved?.[0]?.text) setPersistedText(saved[0].text);
      } catch { /* store not available */ }
    })();
  }, []);

  useEffect(() => {
    if (!IS_TAURI) return;
    const unsub = listen<{ text: string }>("furo://transcription", (e) => {
      if (e.payload.text) setPersistedText(e.payload.text);
    });
    return () => { unsub.then((fn) => fn()); };
  }, []);

  // ── Fullscreen fade ───────────────────────────────────────────
  useEffect(() => {
    if (!IS_TAURI) return;
    const unsub = listen<boolean>("widget-fullscreen", (e) => {
      setIsFullscreen(e.payload);
      if (e.payload) setShowPopup(false);
    });
    return () => { unsub.then((fn) => fn()); };
  }, []);

  // ── Show widget when recording starts ─────────────────────────
  useEffect(() => {
    if (isActive) tauriShow();
  }, [isActive]);

  // ── Window resize: popup needs 80×62, pill-only needs 80×20 ───
  // Resize is async — open: resize immediately (CSS animation has a small
  // delay to let the resize settle). Close: delay resize until animation ends.
  useEffect(() => {
    if (!IS_TAURI) return;
    if (showPopup) {
      invoke("widget_set_size", { width: 80, height: 62 }).catch(() => {});
      return;
    }
    // Shrink window after the close animation finishes
    const id = setTimeout(() => {
      invoke("widget_set_size", { width: 80, height: 20 }).catch(() => {});
    }, 150);
    return () => clearTimeout(id);
  }, [showPopup]);

  // ── Multi-monitor: reposition to cursor's screen ──────────────
  useEffect(() => {
    if (!IS_TAURI) return;
    const checkMonitor = async () => {
      try {
        const { availableMonitors, cursorPosition, getCurrentWindow } =
          await import("@tauri-apps/api/window");
        const { PhysicalPosition } = await import("@tauri-apps/api/dpi");
        const [cursor, monitors] = await Promise.all([cursorPosition(), availableMonitors()]);
        const monitor = monitors.find((m) => {
          const { x, y } = m.position;
          const { width, height } = m.size;
          return cursor.x >= x && cursor.x < x + width && cursor.y >= y && cursor.y < y + height;
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
        const bottomOffset = IS_MAC ? 100 : 60;
        await win.setPosition(
          new PhysicalPosition(
            mx + Math.round((mw - curSize.width) / 2),
            my + mh - curSize.height - Math.round(bottomOffset * scale),
          ),
        );
      } catch { /* ignore in web dev mode */ }
    };
    const timer = setInterval(checkMonitor, 500);
    return () => clearInterval(timer);
  }, []);

  // ── Hover management ──────────────────────────────────────────
  const handleEnter = () => {
    if (hoverTimer.current) { clearTimeout(hoverTimer.current); hoverTimer.current = null; }
    setIsHovered(true);
  };
  const handleLeave = () => {
    hoverTimer.current = setTimeout(() => {
      setIsHovered(false);
      setShowPopup(false);
      if (isHoldingRef.current) {
        isHoldingRef.current = false;
        invoke("widget_hold_release").catch(() => {});
      }
    }, 200);
  };

  // macOS: Rust polling thread drives hover since WKWebView doesn't get
  // mousemove when another app is the key window.
  useEffect(() => {
    if (!IS_MAC) return;
    let unlisten: (() => void) | undefined;
    listen<boolean>("widget-hover", (e) => {
      e.payload ? handleEnter() : handleLeave();
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Mouse-hold dictation (left-click) ─────────────────────────
  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    isHoldingRef.current = true;
    invoke("widget_hold_start").catch(() => {});
  };
  const handleMouseUp = (e: React.MouseEvent) => {
    if (e.button !== 0 || !isHoldingRef.current) return;
    isHoldingRef.current = false;
    invoke("widget_hold_release").catch(() => {});
  };

  // ── Right-click: toggle popup ─────────────────────────────────
  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setShowPopup((p) => !p);
  };

  // ── Box click: copy + re-paste ────────────────────────────────
  const handleBoxClick = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!displayText) return;
    try { await navigator.clipboard.writeText(displayText); } catch { /* ignore */ }
    if (IS_TAURI) invoke("repaste_last", { text: displayText }).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  return (
    <div
      className="fixed inset-0 cursor-default"
      style={{
        opacity: isFullscreen ? 0 : 1,
        pointerEvents: isFullscreen ? "none" : "auto",
        transition: `opacity 500ms ${EASE_OUT}`,
      }}
      onMouseEnter={IS_MAC ? undefined : handleEnter}
      onMouseLeave={IS_MAC ? undefined : handleLeave}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
      onContextMenu={handleContextMenu}
    >
      {/* ── Popup box: reveals bottom-first (grows out of pill top) ── */}
      <div
        className="absolute left-1/2 w-[44px] h-[36px] rounded-2xl border border-white/40 backdrop-blur-xl bg-black/80 shadow-lg shadow-black/30 cursor-pointer select-none flex items-center justify-center"
        style={{
          bottom: "22px",
          transform: "translateX(-50%)",
          opacity: showPopup ? 1 : 0,
          // clip-path wipe: inset(100% top…) = nothing visible (collapses to bottom edge)
          // → inset(0% top…) = fully visible. Bottom of box (near pill) reveals first → grows up.
          clipPath: showPopup
            ? "inset(0% 0% 0% 0% round 12px)"
            : "inset(100% 0% 0% 0% round 12px)",
          // 40ms delay on open lets the Tauri window resize settle first
          transition: showPopup
            ? `opacity ${DURATION} ${EASE_OUT} 40ms, clip-path ${DURATION} ${EASE_OUT} 40ms`
            : `opacity 180ms ${EASE_OUT}, clip-path 180ms ${EASE_OUT}`,
          pointerEvents: showPopup ? "auto" : "none",
        }}
        onClick={handleBoxClick}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div style={{
          color: copied ? "rgba(255,255,255,0.5)" : "rgba(255,255,255,0.35)",
          transition: `color ${DURATION} ${EASE_OUT}`,
        }}>
          {copied ? <CheckIcon /> : <ClipboardIcon />}
        </div>
      </div>

      {/* ── Pill: smooth scale transition, no bounce ─────────── */}
      <div className="absolute inset-x-0 bottom-0 flex items-end justify-center z-10">
        <div
          className="flex items-center justify-center rounded-full border border-white/40 shadow-lg shadow-black/30 backdrop-blur-xl bg-black/80 w-[80px] h-[20px]"
          style={{
            transform: expanded ? "scale(1)" : "scaleX(0.5) scaleY(0.5)",
            transformOrigin: "bottom center",
            transition: `transform ${DURATION} ${EASE_OUT}`,
            willChange: "transform",
          }}
        >
          {/* Visualizer opacity: set directly, no CSS transition to avoid dimming flicker */}
          <div style={{ opacity: expanded ? (isActive ? 1 : 0.5) : 0 }}>
            <AudioVisualizer volume={volume} state={state} />
          </div>
        </div>
      </div>
    </div>
  );
}
