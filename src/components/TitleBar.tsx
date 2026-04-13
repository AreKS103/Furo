import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";

export function TitleBar() {
  const [isMaximized, setIsMaximized] = useState(false);
  const appWindow = getCurrentWindow();

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    appWindow.onResized(async () => {
      setIsMaximized(await appWindow.isMaximized());
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  return (
    <div
      data-tauri-drag-region
      className="absolute top-0 left-[190px] right-0 h-[36px] select-none flex justify-end items-center bg-cream-50 dark:bg-zinc-900 z-[9999]"
    >
      <div 
        data-tauri-drag-region 
        className="flex-1 h-full"
      />
      <div className="flex h-full items-center text-zinc-500 dark:text-zinc-400">
        <button
          onClick={() => appWindow.minimize()}
          title="Minimize"
          className="flex h-full items-center justify-center px-4 transition-colors hover:bg-black/5 dark:hover:bg-white/10"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg">
            <rect x="1" y="6" width="10" height="1" fill="currentColor" />
          </svg>
        </button>
        <button
          onClick={() => appWindow.toggleMaximize()}
          title="Maximize"
          className="flex h-full items-center justify-center px-4 transition-colors hover:bg-black/5 dark:hover:bg-white/10"
        >
          {isMaximized ? (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
              <path d="M11 1.5V8H8.5V9.5H1V3H3.5V1.5H11ZM10 2.5H4.5V7H10V2.5ZM2 4H3.5V8.5H8V8.5H2V4Z" />
            </svg>
          ) : (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg">
              <rect x="1.5" y="1.5" width="9" height="9" stroke="currentColor" strokeWidth="1" />
            </svg>
          )}
        </button>
        <button
          onClick={() => appWindow.close()}
          title="Close"
          className="flex h-full items-center justify-center px-4 transition-colors hover:bg-red-500 hover:text-white"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
            <path d="M6 5.293L10.293 1L11 1.707L6.707 6L11 10.293L10.293 11L6 6.707L1.707 11L1 10.293L5.293 6L1 1.707L1.707 1L6 5.293Z" />
          </svg>
        </button>
      </div>
    </div>
  );
}