import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { FloatingWidget } from "./components/FloatingWidget";
import "./index.css";

// Disable the browser's native right-click context menu (hides "Inspect Element").
document.addEventListener("contextmenu", (e) => e.preventDefault());

/**
 * Route to the correct UI based on the `window` query param.
 *   - ?window=widget  →  floating dictation pill
 *   - (default)       →  settings dashboard
 */
const params = new URLSearchParams(window.location.search);
const windowType = params.get("window");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {windowType === "widget" ? <FloatingWidget /> : <App />}
  </StrictMode>,
);
