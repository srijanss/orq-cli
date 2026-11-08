// A light/dark switch that lives only in the page: it sets an attribute on the document and
// stores nothing and sends nothing. Without a choice the stylesheet follows the device setting.
import { useState } from "react";
import { Icon } from "./icons.jsx";

function deviceTheme() {
  if (typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: light)").matches) return "light";
  return "dark";
}

export function ThemeToggle() {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme || deviceTheme());
  const next = theme === "dark" ? "light" : "dark";
  return (
    <button
      type="button"
      className="theme-toggle"
      data-theme-toggle
      aria-label={"Switch to " + next + " theme"}
      title={"Switch to " + next + " theme"}
      onClick={() => {
        document.documentElement.dataset.theme = next;
        setTheme(next);
      }}
    >
      <Icon name={theme === "dark" ? "sun" : "moon"} />
    </button>
  );
}
