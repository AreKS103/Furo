import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

export type ServerState =
  | "connecting"
  | "loading"
  | "ready"
  | "recording"
  | "processing"
  | "idle"
  | "disconnected";

export interface SocketState {
  state: ServerState;
  message: string;
  lastText: string;
  lastError: string;
  settings: Record<string, string>;
  volume: number;
}

/**
 * Tauri IPC hook — replaces the WebSocket-based useSocket.
 * Listens for furo:// events and provides the same interface.
 */
export function useFuro(): SocketState {
  const [state, setState] = useState<ServerState>("loading");
  const [message, setMessage] = useState("Starting…");
  const [lastText, setLastText] = useState("");
  const [settings, setSettings] = useState<Record<string, string>>({});
  const [volume, setVolume] = useState(0);
  const [lastError, setLastError] = useState("");

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];

    const setup = async () => {
      unlisteners.push(
        await listen<{ state: string; message: string }>(
          "furo://status",
          (event) => {
            setState((event.payload.state as ServerState) ?? "idle");
            if (event.payload.message !== undefined)
              setMessage(event.payload.message);
            if (event.payload.state !== "recording") setVolume(0);
          }
        )
      );

      unlisteners.push(
        await listen<{ text: string }>("furo://transcription", (event) => {
          setLastText(event.payload.text ?? "");
        })
      );

      unlisteners.push(
        await listen<{ data: Record<string, string> }>(
          "furo://settings",
          (event) => {
            setSettings(event.payload.data ?? {});
          }
        )
      );

      unlisteners.push(
        await listen<{ level: number }>("furo://volume", (event) => {
          setVolume(event.payload.level ?? 0);
        })
      );

      unlisteners.push(
        await listen<{ message: string }>("furo://error", (event) => {
          setLastError(event.payload.message ?? "An error occurred.");
        })
      );

      // Fetch initial settings
      try {
        const data = await invoke<Record<string, string>>("get_settings");
        setSettings(data);
      } catch (e) {
        console.error("Failed to fetch initial settings:", e);
      }
    };

    setup();

    return () => {
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, []);

  return { state, message, lastText, lastError, settings, volume };
}
