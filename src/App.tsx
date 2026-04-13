import { useEffect, useState } from "react";
import { Dashboard } from "./components/Dashboard";
import { TitleBar } from "./components/TitleBar";

export function App() {
  const [theme, setTheme] = useState<"dark" | "light">("dark");

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
  }, [theme]);

  // TitleBar is absolutely positioned to cover the top portion without affecting flex bounds
  // The app remains a flex row behind it, with the sidebar hitting the top.
  return (
    <div className="flex h-screen w-full bg-cream-50 text-warm-900 transition-colors duration-300 dark:bg-zinc-900 dark:text-zinc-50 outline-none relative overflow-hidden">
      <TitleBar />
      <Dashboard theme={theme} setTheme={setTheme} />
    </div>
  );
}
