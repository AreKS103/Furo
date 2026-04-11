/**
 * useHistory — Persistent dictation history with 48-hour auto-purge.
 *
 * Uses @tauri-apps/plugin-store for local file-backed JSON storage.
 * Emits "update-tray" events to keep the system tray quick-copy in sync.
 */

import { useCallback, useEffect, useRef, useState } from "react";

const IS_TAURI = "__TAURI_INTERNALS__" in window;
const STORE_FILE = "dictation-history.json";
const STORE_KEY = "dictations";
const STATS_KEY = "cumulative_stats";
const MAX_AGE_MS = 48 * 60 * 60 * 1000; // 48 hours

export interface DictationEntry {
  id: string;
  text: string;
  timestamp: number;
}

/** Cumulative stats that survive history clears. */
export interface CumulativeStats {
  totalWords: number;
  totalDictations: number;
  /** Sorted array of YYYY-MM-DD date strings the user has ever dictated on. */
  activeDays: string[];
}

export function useHistory() {
  const [entries, setEntries] = useState<DictationEntry[]>([]);
  const [cumulativeStats, setCumulativeStats] = useState<CumulativeStats>({
    totalWords: 0,
    totalDictations: 0,
    activeDays: [],
  });
  const entriesRef = useRef<DictationEntry[]>([]);
  const statsRef = useRef<CumulativeStats>({ totalWords: 0, totalDictations: 0, activeDays: [] });
  const storeRef = useRef<unknown>(null);

  // Keep ref in sync with state (avoids stale closures in callbacks).
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  // ── Initialize store, load saved data, purge stale entries ──────
  useEffect(() => {
    if (!IS_TAURI) return;

    let cancelled = false;

    (async () => {
      const { Store } = await import("@tauri-apps/plugin-store");
      const store = await Store.load(STORE_FILE);
      storeRef.current = store;

      // Load cumulative stats
      const savedStats = await store.get<CumulativeStats>(STATS_KEY);
      if (savedStats) {
        if (!cancelled) {
          setCumulativeStats(savedStats);
          statsRef.current = savedStats;
        }
      }

      const saved = await store.get<DictationEntry[]>(STORE_KEY);
      if (!saved || saved.length === 0) {
        if (!cancelled) setEntries([]);
        return;
      }

      // 48-hour purge
      const now = Date.now();
      const fresh = saved.filter((e) => now - e.timestamp < MAX_AGE_MS);

      if (fresh.length !== saved.length) {
        await store.set(STORE_KEY, fresh);
        await store.save();
      }

      if (!cancelled) {
        setEntries(fresh);
        entriesRef.current = fresh;
      }

      // Sync tray with saved entries
      emitTrayUpdate(fresh);
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  // ── Save a new dictation entry ──────────────────────────────────
  const saveEntry = useCallback(async (text: string) => {
    if (!text.trim()) return;

    const entry: DictationEntry = {
      id: crypto.randomUUID(),
      text: text.trim(),
      timestamp: Date.now(),
    };

    const updated = [entry, ...entriesRef.current];
    setEntries(updated);
    entriesRef.current = updated;

    // Update cumulative stats
    const words = entry.text.split(/\s+/).filter(Boolean).length;
    const day = new Date(entry.timestamp).toISOString().slice(0, 10);
    const prev = statsRef.current;
    const newDays = prev.activeDays.includes(day)
      ? prev.activeDays
      : [...prev.activeDays, day].sort();
    const newStats: CumulativeStats = {
      totalWords: prev.totalWords + words,
      totalDictations: prev.totalDictations + 1,
      activeDays: newDays,
    };
    setCumulativeStats(newStats);
    statsRef.current = newStats;

    if (storeRef.current && IS_TAURI) {
      const store = storeRef.current as {
        set: (k: string, v: unknown) => Promise<void>;
        save: () => Promise<void>;
      };
      await store.set(STORE_KEY, updated);
      await store.set(STATS_KEY, newStats);
      await store.save();
    }

    emitTrayUpdate(updated);
  }, []);

  // ── Clear all history (stats are preserved) ─────────────────────
  const clearAll = useCallback(async () => {
    setEntries([]);
    entriesRef.current = [];

    if (storeRef.current && IS_TAURI) {
      const store = storeRef.current as {
        set: (k: string, v: unknown) => Promise<void>;
        save: () => Promise<void>;
      };
      await store.set(STORE_KEY, []);
      // Stats key is intentionally NOT cleared
      await store.save();
    }

    emitTrayUpdate([]);
  }, []);

  return { entries, saveEntry, clearAll, cumulativeStats };
}

// ── Emit "update-tray" event with the 3 most recent texts ─────────
async function emitTrayUpdate(entries: DictationEntry[]) {
  if (!IS_TAURI) return;
  try {
    const { emit } = await import("@tauri-apps/api/event");
    await emit("update-tray", {
      texts: entries.slice(0, 3).map((e) => e.text),
    });
  } catch {
    // Tauri event API unavailable — ignore
  }
}
