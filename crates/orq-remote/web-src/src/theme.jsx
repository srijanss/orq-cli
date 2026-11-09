// A light / system / dark switch. The choice is kept in this browser's local storage (nothing is
// sent anywhere) and applied as soon as this file loads, so a reload keeps it. "System" sets no
// attribute at all, so the stylesheet follows the device setting live.
import { useState } from "react";
import { Icon } from "./icons.jsx";

const KEY = "orq.theme";
const MODES = ["light", "system", "dark"];
const NEXT = { system: "dark", dark: "light", light: "system" };
const LABEL = { light: "Light", system: "System", dark: "Dark" };
const ICON = { light: "sun", system: "monitor", dark: "moon" };

function kept() {
  try {
    const value = window.localStorage.getItem(KEY);
    return MODES.includes(value) ? value : "system";
  } catch (blocked) {
    return "system";
  }
}

function apply(mode) {
  if (mode === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = mode;
}

// What the page is showing right now wins over storage, so blocked storage still works in-page.
function current() {
  const shown = document.documentElement.dataset.theme;
  return MODES.includes(shown) ? shown : "system";
}

apply(kept());

export function ThemeToggle() {
  const [mode, setMode] = useState(current);
  const next = NEXT[mode];
  const label = "Theme: " + LABEL[mode] + ". Switch to " + LABEL[next];
  return (
    <button
      type="button"
      className="theme-toggle"
      data-theme-toggle
      aria-label={label}
      title={label}
      onClick={() => {
        apply(next);
        try {
          if (next === "system") window.localStorage.removeItem(KEY);
          else window.localStorage.setItem(KEY, next);
        } catch (blocked) {
          /* the choice still holds for this page */
        }
        setMode(next);
      }}
    >
      {MODES.map((name) => (
        <span key={name} className="theme-option" data-mode={name} data-active={mode === name ? "" : undefined}><Icon name={ICON[name]} size={16} /></span>
      ))}
    </button>
  );
}
