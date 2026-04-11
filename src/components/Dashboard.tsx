import { useCallback, useEffect, useRef, useState } from "react";
import { useFuro, type ServerState } from "../hooks/useFuro";
import { useHistory, type DictationEntry, type CumulativeStats } from "../hooks/useHistory";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Mic {
  name: string;
  index: number;
}

const APP_VERSION = __APP_VERSION__;

interface DashboardProps {
  theme: "dark" | "light";
  setTheme: (t: "dark" | "light") => void;
}

/* ── Inline SVG icons ─────────────────────────────────────────────── */

function GridIcon({ className = "h-5 w-5" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </svg>
  );
}

function GearIcon({ className = "h-5 w-5" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 01-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z" />
    </svg>
  );
}

function SunIcon({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="5" />
      <line x1="12" y1="1" x2="12" y2="3" /><line x1="12" y1="21" x2="12" y2="23" />
      <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" /><line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
      <line x1="1" y1="12" x2="3" y2="12" /><line x1="21" y1="12" x2="23" y2="12" />
      <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" /><line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
    </svg>
  );
}

function MoonIcon({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor"><path d="M21 12.79A9 9 0 1111.21 3a7 7 0 009.79 9.79z" /></svg>
  );
}

/* ── Status badge ─────────────────────────────────────────────────── */
function StatusBadge({ state }: { state: ServerState }) {
  const map: Record<string, string> = {
    ready: "bg-emerald-500",
    recording: "bg-red-500 animate-pulse",
    processing: "bg-amber-500 animate-pulse",
    loading: "bg-warm-400 animate-pulse",
    idle: "bg-emerald-500",
    disconnected: "bg-warm-400",
    connecting: "bg-warm-400 animate-pulse",
  };
  return <span className={`inline-block h-2 w-2 rounded-full ${map[state] ?? "bg-warm-400"}`} />;
}

/* ── Animated counter hook ────────────────────────────────────────── */
function useCountUp(target: number, duration = 600): number {
  const [display, setDisplay] = useState(target);
  const prevRef = useRef(target);
  const rafRef = useRef(0);

  useEffect(() => {
    const from = prevRef.current;
    const to = target;
    prevRef.current = target;
    if (from === to) return;

    const start = performance.now();
    const step = (now: number) => {
      const elapsed = now - start;
      const progress = Math.min(elapsed / duration, 1);
      // ease-out cubic
      const eased = 1 - Math.pow(1 - progress, 3);
      setDisplay(Math.round(from + (to - from) * eased));
      if (progress < 1) {
        rafRef.current = requestAnimationFrame(step);
      }
    };
    rafRef.current = requestAnimationFrame(step);
    return () => cancelAnimationFrame(rafRef.current);
  }, [target, duration]);

  return display;
}

/* ── Date grouping helpers ────────────────────────────────────────── */
function getDateLabel(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const entry = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = Math.floor((today.getTime() - entry.getTime()) / 86400000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric" });
}

function groupByDate(entries: DictationEntry[]): [string, DictationEntry[]][] {
  const groups = new Map<string, DictationEntry[]>();
  for (const e of entries) {
    const label = getDateLabel(e.timestamp);
    const arr = groups.get(label) ?? [];
    arr.push(e);
    groups.set(label, arr);
  }
  return Array.from(groups.entries());
}

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString("en-US", { hour: "2-digit", minute: "2-digit" });
}

/* ── Stats helpers ────────────────────────────────────────────────── */

function dayStreak(activeDays: string[]): number {
  if (activeDays.length === 0) return 0;
  const days = new Set(activeDays);
  const today = new Date();
  let streak = 0;
  for (let i = 0; i < 365; i++) {
    const d = new Date(today.getFullYear(), today.getMonth(), today.getDate() - i);
    const key = d.toISOString().slice(0, 10);
    if (days.has(key)) {
      streak++;
    } else {
      break;
    }
  }
  return streak;
}

/* ── Whisper supported languages ──────────────────────────────────── */
const WHISPER_LANGUAGES: { code: string; name: string }[] = [
  { code: "en", name: "English" },
  { code: "zh", name: "Chinese" },
  { code: "de", name: "German" },
  { code: "es", name: "Spanish" },
  { code: "ru", name: "Russian" },
  { code: "ko", name: "Korean" },
  { code: "fr", name: "French" },
  { code: "ja", name: "Japanese" },
  { code: "pt", name: "Portuguese" },
  { code: "tr", name: "Turkish" },
  { code: "pl", name: "Polish" },
  { code: "ca", name: "Catalan" },
  { code: "nl", name: "Dutch" },
  { code: "ar", name: "Arabic" },
  { code: "sv", name: "Swedish" },
  { code: "it", name: "Italian" },
  { code: "id", name: "Indonesian" },
  { code: "hi", name: "Hindi" },
  { code: "fi", name: "Finnish" },
  { code: "vi", name: "Vietnamese" },
  { code: "he", name: "Hebrew" },
  { code: "uk", name: "Ukrainian" },
  { code: "el", name: "Greek" },
  { code: "ms", name: "Malay" },
  { code: "cs", name: "Czech" },
  { code: "ro", name: "Romanian" },
  { code: "da", name: "Danish" },
  { code: "hu", name: "Hungarian" },
  { code: "ta", name: "Tamil" },
  { code: "no", name: "Norwegian" },
  { code: "th", name: "Thai" },
  { code: "ur", name: "Urdu" },
  { code: "hr", name: "Croatian" },
  { code: "bg", name: "Bulgarian" },
  { code: "lt", name: "Lithuanian" },
  { code: "la", name: "Latin" },
  { code: "mi", name: "Maori" },
  { code: "ml", name: "Malayalam" },
  { code: "cy", name: "Welsh" },
  { code: "sk", name: "Slovak" },
  { code: "te", name: "Telugu" },
  { code: "fa", name: "Persian" },
  { code: "lv", name: "Latvian" },
  { code: "bn", name: "Bengali" },
  { code: "sr", name: "Serbian" },
  { code: "az", name: "Azerbaijani" },
  { code: "sl", name: "Slovenian" },
  { code: "kn", name: "Kannada" },
  { code: "et", name: "Estonian" },
  { code: "mk", name: "Macedonian" },
  { code: "br", name: "Breton" },
  { code: "eu", name: "Basque" },
  { code: "is", name: "Icelandic" },
  { code: "hy", name: "Armenian" },
  { code: "ne", name: "Nepali" },
  { code: "mn", name: "Mongolian" },
  { code: "bs", name: "Bosnian" },
  { code: "kk", name: "Kazakh" },
  { code: "sq", name: "Albanian" },
  { code: "sw", name: "Swahili" },
  { code: "gl", name: "Galician" },
  { code: "mr", name: "Marathi" },
  { code: "pa", name: "Punjabi" },
  { code: "si", name: "Sinhala" },
  { code: "km", name: "Khmer" },
  { code: "sn", name: "Shona" },
  { code: "yo", name: "Yoruba" },
  { code: "so", name: "Somali" },
  { code: "af", name: "Afrikaans" },
  { code: "oc", name: "Occitan" },
  { code: "ka", name: "Georgian" },
  { code: "be", name: "Belarusian" },
  { code: "tg", name: "Tajik" },
  { code: "sd", name: "Sindhi" },
  { code: "gu", name: "Gujarati" },
  { code: "am", name: "Amharic" },
  { code: "yi", name: "Yiddish" },
  { code: "lo", name: "Lao" },
  { code: "uz", name: "Uzbek" },
  { code: "fo", name: "Faroese" },
  { code: "ht", name: "Haitian Creole" },
  { code: "ps", name: "Pashto" },
  { code: "tk", name: "Turkmen" },
  { code: "nn", name: "Nynorsk" },
  { code: "mt", name: "Maltese" },
  { code: "sa", name: "Sanskrit" },
  { code: "lb", name: "Luxembourgish" },
  { code: "my", name: "Myanmar" },
  { code: "bo", name: "Tibetan" },
  { code: "tl", name: "Tagalog" },
  { code: "mg", name: "Malagasy" },
  { code: "as", name: "Assamese" },
  { code: "tt", name: "Tatar" },
  { code: "haw", name: "Hawaiian" },
  { code: "ln", name: "Lingala" },
  { code: "ha", name: "Hausa" },
  { code: "ba", name: "Bashkir" },
  { code: "jw", name: "Javanese" },
  { code: "su", name: "Sundanese" },
  { code: "yue", name: "Cantonese" },
  { code: "auto", name: "Auto-detect" },
];

/* ── Home page: stats + date-grouped history ──────────────────────── */
function HomePage({
  entries,
  holdHotkey,
  handsfreeHotkey,
  onClear,
  cumulativeStats,
}: {
  entries: DictationEntry[];
  holdHotkey: string;
  handsfreeHotkey: string;
  onClear: () => void;
  cumulativeStats: CumulativeStats;
}) {
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const animWords = useCountUp(cumulativeStats.totalWords);
  const animDictations = useCountUp(cumulativeStats.totalDictations);
  const animStreak = useCountUp(dayStreak(cumulativeStats.activeDays));

  const copyText = async (entry: DictationEntry) => {
    try {
      await navigator.clipboard.writeText(entry.text);
      setCopiedId(entry.id);
      setTimeout(() => setCopiedId(null), 1500);
    } catch (e) {
      console.warn("[clipboard] copy failed:", e);
    }
  };

  const groups = groupByDate(entries);
  const holdLabel = holdHotkey ? holdHotkey.replace(/\+/g, " + ").toUpperCase() : "F9";
  const hfLabel = handsfreeHotkey ? handsfreeHotkey.replace(/\+/g, " + ").toUpperCase() : "F10";

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="px-8 py-8">
        {/* Logo image */}
        {/* Welcome header */}
        <h1 className="mb-6 font-serif text-[26px] font-semibold text-warm-900 dark:text-zinc-50">
          Welcome back
        </h1>

        {/* Hero banner — full-width image with overlay */}
        <div className="relative mb-8 overflow-hidden rounded-2xl shadow-md">
          <img
            src="/HeroImage.png"
            className="h-[180px] w-full object-cover"
            alt="Furo voice dictation"
            draggable={false}
          />
          {/* Dark gradient scrim — heavier on right so stats are readable */}
          <div className="absolute inset-0 bg-gradient-to-r from-black/60 via-black/30 to-black/55" />

          {/* Left — hotkey text */}
          <div className="absolute bottom-0 left-0 p-6">
            <p className="text-lg font-semibold leading-snug text-white drop-shadow">
              Hold{" "}
              <span className="rounded-md bg-white/20 px-1.5 py-0.5 font-mono text-sm backdrop-blur-sm">
                {holdLabel}
              </span>{" "}
              to dictate
            </p>
            <p className="mt-1 text-sm text-white/75 drop-shadow">
              Or press{" "}
              <span className="font-mono font-medium text-white/95">{hfLabel}</span>{" "}
              for hands-free
            </p>
          </div>

          {/* Right — stats */}
          <div className="absolute bottom-0 right-0 flex flex-col gap-1 p-6 text-right">
            <div>
              <span className="text-2xl font-semibold tabular-nums text-white drop-shadow">
                {animWords.toLocaleString()}
              </span>
              <span className="ml-1.5 text-xs text-white/65">words</span>
            </div>
            <div>
              <span className="text-2xl font-semibold tabular-nums text-white drop-shadow">
                {animDictations}
              </span>
              <span className="ml-1.5 text-xs text-white/65">dictations</span>
            </div>
            <div>
              <span className="text-2xl font-semibold tabular-nums text-white drop-shadow">
                {animStreak}
              </span>
              <span className="ml-1.5 text-xs text-white/65">day streak</span>
            </div>
          </div>
        </div>

        {/* Date-grouped history */}
        {entries.length === 0 ? (
          <p className="pt-16 text-center text-sm text-warm-600 dark:text-zinc-400">
            No dictations yet. Your history will appear here.
          </p>
        ) : (
          <div className="flex flex-col gap-6">
            {/* Clear all */}
            <div className="flex justify-end">
              <button
                onClick={onClear}
                className="rounded-lg px-3 py-1.5 text-xs font-medium text-red-400 transition hover:bg-red-500/10"
              >
                Clear All
              </button>
            </div>

            {groups.map(([label, group]) => (
              <div key={label}>
                <h3 className="mb-2 text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
                  {label}
                </h3>
                <div className="overflow-hidden rounded-xl border border-cream-300 bg-white dark:border-zinc-700 dark:bg-zinc-800">
                  {group.map((entry, i) => (
                    <div
                      key={entry.id}
                      onClick={() => copyText(entry)}
                      className={`group flex cursor-pointer items-start gap-6 px-5 py-3.5 transition hover:bg-cream-100 dark:hover:bg-zinc-700 ${
                        i > 0 ? "border-t border-cream-200 dark:border-zinc-700" : ""
                      }`}
                    >
                      <span className="mt-0.5 w-[80px] flex-shrink-0 text-[13px] tabular-nums text-warm-400 dark:text-zinc-500">
                        {formatTime(entry.timestamp)}
                      </span>
                      <span className="flex-1 text-[14px] leading-relaxed text-warm-800 dark:text-zinc-100">
                        {entry.text}
                      </span>
                      <span className="mt-0.5 text-[11px] font-medium text-warm-300 opacity-0 transition group-hover:opacity-100 dark:text-zinc-500">
                        {copiedId === entry.id ? "Copied!" : "Copy"}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Settings page ────────────────────────────────────────────────── */
function SettingsPage({
  theme,
  setTheme,
  saveSetting,
  onCheckUpdate,
  updateCheckMsg,
}: {
  theme: "dark" | "light";
  setTheme: (t: "dark" | "light") => void;
  saveSetting: (key: string, value: string) => Promise<void>;
  onCheckUpdate: () => void;
  updateCheckMsg: string;
}) {
  const { settings } = useFuro();
  const [mics, setMics] = useState<Mic[]>([]);
  const [selectedMic, setSelectedMic] = useState("");
  const [holdHotkey, setHoldHotkey] = useState("");
  const [handsfreeHotkey, setHandsfreeHotkey] = useState("");
  const [rebindingHold, setRebindingHold] = useState(false);
  const [rebindingHandsfree, setRebindingHandsfree] = useState(false);
  const [autostart, setAutostart] = useState(false);
  const [language, setLanguage] = useState("en");
  const [soundEnabled, setSoundEnabled] = useState(true);
  const [soundVolume, setSoundVolume] = useState(5); // 0–100 integer, stored as 0.0–1.0

  useEffect(() => {
    if (settings.microphone !== undefined) setSelectedMic(settings.microphone);
    if (settings.hotkey_hold !== undefined) setHoldHotkey(settings.hotkey_hold);
    if (settings.hotkey_handsfree !== undefined) setHandsfreeHotkey(settings.hotkey_handsfree);
    if (settings.language !== undefined) setLanguage(settings.language);
    if (settings.sound_enabled !== undefined) setSoundEnabled(settings.sound_enabled !== "false");
    if (settings.sound_volume !== undefined) {
      // Perceptual curve inverse: pct = sqrt(stored / 0.20) × 100
      const stored = parseFloat(settings.sound_volume);
      const pct = Math.round(Math.sqrt(stored / 0.20) * 100);
      setSoundVolume(Number.isFinite(pct) ? Math.min(pct, 100) : 50);
    }
  }, [settings]);

  useEffect(() => {
    invoke<Mic[]>("list_microphones").then((d) => setMics(d ?? [])).catch((e) => console.warn("[settings] list mics:", e));
    invoke<boolean>("get_autostart").then(setAutostart).catch((e) => console.warn("[settings] get autostart:", e));
  }, []);

  /* ── Rebind effect ───────────────────────────────────────────────
   *
   * Rebind capture is handled entirely on the Rust side so that OS-level
   * shortcuts (Win key → Start menu, Win+Space → language switcher) are
   * suppressed while the user is assigning a new hotkey.
   *
   * Flow:
   *  1. invoke("set_rebind_mode", { active: true })  — hook enters sniff mode
   *  2. Rust worker collects pressed VK codes, builds combo on key-up
   *  3. Hook emits `furo://rebind-capture` with the combo string
   *  4. Frontend receives it, saves, clears rebind state
   *  5. invoke("set_rebind_mode", { active: false })  — hook exits sniff mode
   *
   * Mouse buttons are still captured on the frontend side (no OS conflict).
   * Escape cancels the rebind without changing the saved key.
   */
  const useRebindEffect = (
    active: boolean,
    setActive: (v: boolean) => void,
    setKey: (v: string) => void,
    settingName: string,
  ) => {
    useEffect(() => {
      if (!active) return;

      // Tell the Rust hook to enter rebind sniff mode (suppresses Win key etc.)
      invoke("set_rebind_mode", { active: true });

      let unlisten: (() => void) | undefined;

      const finish = (combo: string) => {
        invoke("set_rebind_mode", { active: false });
        setKey(combo);
        setActive(false);
        saveSetting(settingName, combo);
        unlisten?.();
      };

      const cancel = () => {
        invoke("set_rebind_mode", { active: false });
        setActive(false);
        unlisten?.();
      };

      // Backend-captured combo (keyboard, including Win combos)
      listen<string>("furo://rebind-capture", (event) => {
        finish(event.payload);
      }).then((fn) => { unlisten = fn; });

      // Frontend still handles mouse buttons (no OS conflict there)
      const mh = (e: MouseEvent) => {
        const hasModifier = e.ctrlKey || e.metaKey || e.altKey || e.shiftKey;
        if ((e.button === 0 || e.button === 2) && !hasModifier) return;
        e.preventDefault();
        const parts: string[] = [];
        if (e.ctrlKey) parts.push("ctrl");
        if (e.metaKey) parts.push("win");
        if (e.altKey) parts.push("alt");
        if (e.shiftKey) parts.push("shift");
        const bm: Record<number, string> = { 0: "mouse1", 1: "mouse3", 2: "mouse2", 3: "mouse4", 4: "mouse5" };
        parts.push(bm[e.button] ?? `mouse${e.button + 1}`);
        finish(parts.join("+"));
      };

      // Escape cancels
      const kd = (e: KeyboardEvent) => {
        if (e.code === "Escape") { e.preventDefault(); cancel(); }
      };

      window.addEventListener("keydown", kd, true);
      window.addEventListener("mousedown", mh);
      return () => {
        window.removeEventListener("keydown", kd, true);
        window.removeEventListener("mousedown", mh);
        invoke("set_rebind_mode", { active: false });
        unlisten?.();
      };
    }, [active, setActive, setKey, settingName]);
  };

  useRebindEffect(rebindingHold, setRebindingHold, setHoldHotkey, "hotkey_hold");
  useRebindEffect(rebindingHandsfree, setRebindingHandsfree, setHandsfreeHotkey, "hotkey_handsfree");

  const toggleAutostart = async () => {
    const next = !autostart;
    try {
      await invoke("set_autostart", { enabled: next });
      setAutostart(next);
    } catch (e) {
      console.warn("[settings] set autostart:", e);
    }
  };

  const inputCls =
    "w-full rounded-xl border border-cream-300 bg-white px-3.5 py-2.5 text-sm text-warm-800 shadow-sm outline-none transition focus:border-warm-400 focus:ring-2 focus:ring-warm-300/30 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100 dark:focus:border-zinc-500";

  const btnCls =
    "rounded-xl bg-warm-800 px-4 py-2.5 text-sm font-medium text-white shadow-sm transition hover:bg-warm-700 active:scale-[0.97] dark:bg-zinc-200 dark:text-zinc-900 dark:hover:bg-zinc-300";

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="mx-auto max-w-xl px-8 py-8">
        <h1 className="mb-8 font-serif text-[26px] font-semibold text-warm-900 dark:text-zinc-50">
          Settings
        </h1>

        <div className="flex flex-col gap-7">
          {/* Microphone */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Input Device
            </label>
            <select
              value={selectedMic}
              onChange={(e) => {
                setSelectedMic(e.target.value);
                saveSetting("microphone", e.target.value);
              }}
              className={inputCls + " cursor-pointer"}
            >
              <option value="">System Default</option>
              {mics.map((m) => (
                <option key={m.index} value={m.name}>
                  {m.name}
                </option>
              ))}
            </select>
          </section>

          {/* Language */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Language
            </label>
            <select
              value={language}
              onChange={(e) => {
                setLanguage(e.target.value);
                saveSetting("language", e.target.value);
              }}
              className={inputCls + " cursor-pointer"}
            >
              {WHISPER_LANGUAGES.map((l) => (
                <option key={l.code} value={l.code}>
                  {l.name}
                </option>
              ))}
            </select>
            <p className="mt-1.5 text-[11px] text-warm-400 dark:text-zinc-500">
              Transcription language. Use auto-detect for multilingual input.
            </p>
          </section>

          {/* Hold Hotkey */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Hold Hotkey
            </label>
            <div className="flex items-center gap-3">
              <span className={inputCls + " flex-1 font-medium"}>
                {rebindingHold ? (
                  <span className="animate-pulse text-warm-400">Press any key combo...</span>
                ) : (
                  (holdHotkey || "F9").replace(/\+/g, " + ").toUpperCase()
                )}
              </span>
              <button onMouseDown={(e) => { e.preventDefault(); setRebindingHold(true); }} className={btnCls}>
                Rebind
              </button>
            </div>
            <p className="mt-1.5 text-[11px] text-warm-400 dark:text-zinc-500">
              Hold to record, release to stop
            </p>
          </section>

          {/* Hands-free Hotkey */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Hands-free Hotkey
            </label>
            <div className="flex items-center gap-3">
              <span className={inputCls + " flex-1 font-medium"}>
                {rebindingHandsfree ? (
                  <span className="animate-pulse text-warm-400">Press any key combo...</span>
                ) : (
                  (handsfreeHotkey || "F10").replace(/\+/g, " + ").toUpperCase()
                )}
              </span>
              <button onMouseDown={(e) => { e.preventDefault(); setRebindingHandsfree(true); }} className={btnCls}>
                Rebind
              </button>
            </div>
            <p className="mt-1.5 text-[11px] text-warm-400 dark:text-zinc-500">
              Press to start, press again to stop
            </p>
          </section>

          {/* UI Sound */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              UI Sound
            </label>
            {/* Enabled toggle */}
            <button
              onClick={() => {
                const next = !soundEnabled;
                setSoundEnabled(next);
                saveSetting("sound_enabled", next ? "true" : "false");
              }}
              className="flex w-full items-center justify-between rounded-xl border border-cream-300 bg-white px-3.5 py-3 text-sm shadow-sm transition dark:border-zinc-700 dark:bg-zinc-800"
            >
              <span className="text-warm-800 dark:text-zinc-100">Play sound on keybind</span>
              <span
                className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                  soundEnabled
                    ? "bg-warm-800 dark:bg-zinc-200"
                    : "bg-cream-300 dark:bg-zinc-600"
                }`}
              >
                <span
                  className={`inline-block h-3.5 w-3.5 rounded-full bg-white transition-transform ${
                    soundEnabled ? "translate-x-4" : "translate-x-0.5"
                  }`}
                />
              </span>
            </button>
            {/* Volume slider — shown only when enabled */}
            {soundEnabled && (
              <div className="mt-3 rounded-xl border border-cream-300 bg-white px-3.5 py-3 shadow-sm dark:border-zinc-700 dark:bg-zinc-800">
                <div className="mb-2 flex items-center justify-between">
                  <span className="text-sm text-warm-800 dark:text-zinc-100">Volume</span>
                  <span className="text-sm font-medium tabular-nums text-warm-600 dark:text-zinc-300">
                    {soundVolume}%
                  </span>
                </div>
                <input
                  type="range"
                  min={0}
                  max={100}
                  step={1}
                  value={soundVolume}
                  onChange={(e) => setSoundVolume(Number(e.target.value))}
                  onPointerUp={(e) => {
                    const pct = Number((e.target as HTMLInputElement).value);
                    // Perceptual curve: stored = (pct/100)² × 0.20  → max amplitude 0.20 at 100%
                    const stored = ((pct / 100) ** 2 * 0.20).toFixed(4);
                    saveSetting("sound_volume", stored).then(() => {
                      invoke("preview_sound").catch(() => {});
                    });
                  }}
                  className="w-full accent-warm-800 dark:accent-zinc-200"
                />
                <p className="mt-1.5 text-[11px] text-warm-400 dark:text-zinc-500">
                  Release the slider to preview the sound
                </p>
              </div>
            )}
          </section>

          {/* Open on Startup */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Startup
            </label>
            <button
              onClick={toggleAutostart}
              className="flex w-full items-center justify-between rounded-xl border border-cream-300 bg-white px-3.5 py-3 text-sm shadow-sm transition dark:border-zinc-700 dark:bg-zinc-800"
            >
              <span className="text-warm-800 dark:text-zinc-100">Open on startup</span>
              <span
                className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                  autostart
                    ? "bg-warm-800 dark:bg-zinc-200"
                    : "bg-cream-300 dark:bg-zinc-600"
                }`}
              >
                <span
                  className={`inline-block h-3.5 w-3.5 rounded-full bg-white transition-transform ${
                    autostart ? "translate-x-4" : "translate-x-0.5"
                  }`}
                />
              </span>
            </button>
          </section>

          {/* Theme */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              Appearance
            </label>
            <button
              onClick={() => {
                const n = theme === "dark" ? "light" : "dark";
                setTheme(n);
                saveSetting("theme", n);
              }}
              className="flex w-full items-center gap-3 rounded-xl border border-cream-300 bg-white px-3.5 py-3 text-sm shadow-sm transition hover:border-warm-300 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:border-zinc-500"
            >
              {theme === "dark" ? <SunIcon /> : <MoonIcon />}
              <span className="text-warm-800 dark:text-zinc-100">
                {theme === "dark" ? "Switch to Light Mode" : "Switch to Dark Mode"}
              </span>
            </button>
          </section>

          {/* About / Updates */}
          <section>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-warm-400 dark:text-zinc-500">
              About
            </label>
            <div className="rounded-xl border border-cream-300 bg-white px-3.5 py-3 shadow-sm dark:border-zinc-700 dark:bg-zinc-800">
              <div className="flex items-center justify-between">
                <div>
                  <span className="text-sm font-medium text-warm-800 dark:text-zinc-100">Furo</span>
                  <span className="ml-2 text-xs text-warm-400 dark:text-zinc-500">v{APP_VERSION}</span>
                </div>
                <button
                  onClick={onCheckUpdate}
                  disabled={updateCheckMsg === "Checking…"}
                  className="rounded-lg border border-cream-300 px-3 py-1.5 text-[12px] font-medium text-warm-700 transition hover:bg-cream-100 disabled:opacity-50 dark:border-zinc-600 dark:text-zinc-300 dark:hover:bg-zinc-700"
                >
                  {updateCheckMsg === "Checking…" ? "Checking…" : "Check for Updates"}
                </button>
              </div>
              {updateCheckMsg && updateCheckMsg !== "Checking…" && (
                <p className="mt-2 text-[12px] text-warm-500 dark:text-zinc-400">{updateCheckMsg}</p>
              )}
            </div>
          </section>
        </div>

        {/* Version footer */}
        <div className="mt-6 pb-2 text-center">
          <span className="text-[11px] text-warm-300 dark:text-zinc-600">Furo v{APP_VERSION}</span>
        </div>
      </div>
    </div>
  );
}

/* ════════════════════════════════════════════════════════════════════ */
export function Dashboard({ theme, setTheme }: DashboardProps) {
  const { state, message, settings, lastText, lastError } = useFuro();
  const { entries, saveEntry, clearAll, cumulativeStats } = useHistory();
  const [activeTab, setActiveTab] = useState<"home" | "settings">("home");
  const lastSavedRef = useRef("");
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<"idle" | "downloading" | "ready">("idle");
  const [updateCheckMsg, setUpdateCheckMsg] = useState("");
  const updateRef = useRef<import("@tauri-apps/plugin-updater").Update | null>(null);

  const holdHotkey = settings.hotkey_hold ?? "";
  const handsfreeHotkey = settings.hotkey_handsfree ?? "";

  // Apply saved theme as soon as settings load — runs regardless of which tab is active.
  useEffect(() => {
    if (settings.theme) setTheme(settings.theme as "dark" | "light");
  }, [settings.theme, setTheme]);

  /* Check for updates on mount + when triggered from tray */
  const checkForUpdate = useCallback(async (manual = false) => {
    try {
      if (manual) setUpdateCheckMsg("Checking…");
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        updateRef.current = update;
        setUpdateAvailable(true);
        if (manual) setUpdateCheckMsg("");
      } else if (manual) {
        setUpdateCheckMsg("You're on the latest version.");
        setTimeout(() => setUpdateCheckMsg(""), 4000);
      }
    } catch (e) {
      if (manual) {
        setUpdateCheckMsg("Update check failed — check your connection.");
        setTimeout(() => setUpdateCheckMsg(""), 5000);
      }
      console.warn("[updater]", e);
    }
  }, []);

  // Auto-check on mount
  useEffect(() => {
    checkForUpdate();
  }, [checkForUpdate]);

  // Periodic auto-check every 30 minutes
  useEffect(() => {
    const id = setInterval(() => checkForUpdate(), 30 * 60 * 1000);
    return () => clearInterval(id);
  }, [checkForUpdate]);

  // Listen for tray "Check for Update" event
  useEffect(() => {
    const unlisten = listen("furo://check-update", () => {
      checkForUpdate(true);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, [checkForUpdate]);

  const installUpdate = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;
    try {
      setUpdateStatus("downloading");
      await update.downloadAndInstall();
      setUpdateStatus("ready");
      // Tauri will restart automatically after install
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (e) {
      setUpdateStatus("idle");
      console.warn("[updater] install failed:", e);
    }
  }, []);

  /* Save new transcriptions to history */
  useEffect(() => {
    if (lastText && lastText.trim() && lastText !== lastSavedRef.current) {
      lastSavedRef.current = lastText;
      saveEntry(lastText);
    }
  }, [lastText, saveEntry]);

  /* Native OS notification on error */
  useEffect(() => {
    if (!lastError) return;
    if (Notification.permission === "granted") {
      new Notification("Furo", { body: lastError });
    } else if (Notification.permission !== "denied") {
      Notification.requestPermission().then((p) => {
        if (p === "granted") new Notification("Furo", { body: lastError });
      });
    }
  }, [lastError]);

  const saveSetting = useCallback(async (key: string, value: string) => {
    await invoke("update_settings", { data: { [key]: value } }).catch((e) => console.warn("[settings] save:", e));
  }, []);

  const navItem = (
    id: "home" | "settings",
    label: string,
    Icon: React.FC<{ className?: string }>,
  ) => (
    <button
      onClick={() => setActiveTab(id)}
      className={`flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-[14px] font-medium transition ${
        activeTab === id
          ? "bg-cream-200 text-warm-900 dark:bg-zinc-800 dark:text-zinc-50"
          : "text-warm-500 hover:bg-cream-100 hover:text-warm-700 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-zinc-100"
      }`}
    >
      <Icon className="h-[18px] w-[18px] flex-shrink-0" />
      <span>{label}</span>
    </button>
  );

  return (
    <>
      {/* ── Left sidebar ──────────────────────────────────────────── */}
      <aside className="flex w-[190px] flex-shrink-0 flex-col border-r border-cream-200 bg-cream-100 dark:border-zinc-800 dark:bg-zinc-950">
        {/* Logo */}
        <div className="flex h-14 items-center gap-2 px-4" data-tauri-drag-region>
          <svg
            className="h-6 w-6 text-warm-800 dark:text-zinc-100"
            viewBox="0 0 24 24"
            fill="currentColor"
          >
            <rect x="2" y="8" width="2.5" height="8" rx="1.25" />
            <rect x="7" y="5" width="2.5" height="14" rx="1.25" />
            <rect x="12" y="3" width="2.5" height="18" rx="1.25" />
            <rect x="17" y="6" width="2.5" height="12" rx="1.25" />
          </svg>
          <span className="font-serif text-[20px] font-semibold text-warm-900 dark:text-zinc-50">
            Furo
          </span>
        </div>

        {/* Top nav */}
        <nav className="flex flex-1 flex-col gap-1 px-3 pt-1">
          {navItem("home", "Home", GridIcon)}
        </nav>

        {/* Bottom nav */}
        <div className="flex flex-col gap-1 border-t border-cream-200 px-3 py-3 dark:border-zinc-800">
          {navItem("settings", "Settings", GearIcon)}
          {/* Status */}
          <div className="mt-1 flex items-center gap-2 px-3 py-1">
            <StatusBadge state={state} />
            <span className="truncate text-[11px] text-warm-400 dark:text-zinc-500">
              {message || stateLabel(state)}
            </span>
          </div>
        </div>
      </aside>

      {/* ── Main content ──────────────────────────────────────────── */}
      <main className="flex flex-1 flex-col overflow-hidden bg-cream-50 dark:bg-zinc-900">
        {/* Update banner */}
        {updateAvailable && (
          <div className="flex items-center justify-between border-b border-emerald-200 bg-emerald-50 px-4 py-2.5 dark:border-emerald-800 dark:bg-emerald-950/50">
            <span className="text-[13px] font-medium text-emerald-800 dark:text-emerald-300">
              {updateStatus === "downloading"
                ? "Downloading update…"
                : updateStatus === "ready"
                  ? "Restarting…"
                  : `A new version of Furo is available (v${updateRef.current?.version ?? ""})`}
            </span>
            {updateStatus === "idle" && (
              <button
                onClick={installUpdate}
                className="rounded-lg bg-emerald-600 px-3 py-1 text-[12px] font-medium text-white transition hover:bg-emerald-700"
              >
                Install now
              </button>
            )}
          </div>
        )}
        {activeTab === "home" ? (
          <HomePage
            entries={entries}
            holdHotkey={holdHotkey}
            handsfreeHotkey={handsfreeHotkey}
            onClear={clearAll}
            cumulativeStats={cumulativeStats}
          />
        ) : (
          <SettingsPage
            theme={theme}
            setTheme={setTheme}
            saveSetting={saveSetting}
            onCheckUpdate={() => checkForUpdate(true)}
            updateCheckMsg={updateCheckMsg}
          />
        )}
      </main>
    </>
  );
}

function stateLabel(s: ServerState): string {
  switch (s) {
    case "connecting":
      return "Connecting...";
    case "disconnected":
      return "Disconnected";
    case "loading":
      return "Loading...";
    case "ready":
    case "idle":
      return "Ready";
    case "recording":
      return "Listening...";
    case "processing":
      return "Processing...";
    default:
      return "";
  }
}
