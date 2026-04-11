import { useEffect, useRef, useState } from "react";
import { useFuro, type ServerState } from "../hooks/useFuro";
import { invoke } from "@tauri-apps/api/core";

const IS_TAURI = "__TAURI_INTERNALS__" in window;


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
  const { state, volume } = useFuro();
  const isActive = state === "recording" || state === "processing";
  const [isHovered, setIsHovered] = useState(false);
  const expanded = isActive || isHovered;
  const lastMonitorIdRef = useRef<string>("");
  const isHoldingRef = useRef(false);

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
    if (expanded) {
      invoke("widget_set_expanded", { expanded: true }).catch(() => {});
      return;
    }
    const timer = setTimeout(() => {
      invoke("widget_set_expanded", { expanded: false }).catch(() => {});
    }, 210);
    return () => clearTimeout(timer);
  }, [expanded]);

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
        await win.setPosition(
          new PhysicalPosition(
            mx + Math.round((mw - wW) / 2),
            my + mh - wH - Math.round(60 * scale),
          ),
        );
      } catch {
        // Tauri APIs unavailable in web dev mode — ignore
      }
    };

    const timer = setInterval(checkMonitor, 500);
    return () => clearInterval(timer);
  }, []);

  // ── Mouse-hold dictation: press and hold to record ──────────
  const handleMouseDown = () => {
    isHoldingRef.current = true;
    invoke("widget_hold_start").catch(() => {});
  };
  const handleMouseUp = () => {
    if (!isHoldingRef.current) return;
    isHoldingRef.current = false;
    invoke("widget_hold_release").catch(() => {});
  };

  return (
    <div
      className="fixed inset-0 flex items-center justify-center cursor-pointer"
      onMouseEnter={() => setIsHovered(true)}
      onMouseLeave={() => {
        setIsHovered(false);
        if (isHoldingRef.current) {
          isHoldingRef.current = false;
          invoke("widget_hold_release").catch(() => {});
        }
      }}
      onMouseDown={handleMouseDown}
      onMouseUp={handleMouseUp}
    >
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
  );
}
