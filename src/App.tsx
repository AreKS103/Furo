import { useEffect, useState } from "react";
import { Dashboard } from "./components/Dashboard";

export function App() {
  const [theme, setTheme] = useState<"dark" | "light">("dark");

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark");
  }, [theme]);

  return (
    <div className="flex h-screen bg-cream-50 text-warm-900 transition-colors duration-300 dark:bg-zinc-900 dark:text-zinc-50">
      <Dashboard theme={theme} setTheme={setTheme} />
    </div>
  );
}
