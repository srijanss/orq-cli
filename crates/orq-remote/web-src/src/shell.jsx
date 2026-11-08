// The page frame: a sidebar with the Dashboard, the chosen project's sections and Settings.
// The place lives in the address hash, so Back and Forward move it. On a phone the sidebar is a
// drawer that behaves like a dialog (Escape, scrim or close button close it, focus is trapped and
// returned, page scroll is locked); on a desktop the same sidebar collapses to an icon rail.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";

function subscribeToHash(notify) {
  window.addEventListener("hashchange", notify);
  return () => window.removeEventListener("hashchange", notify);
}

const currentHash = () => window.location.hash;

export function navigationEntries(projectId) {
  const entries = [{ label: "Dashboard", icon: "▦", href: "#/dashboard" }];
  if (typeof projectId === "string" && projectId !== "") {
    const base = `#/projects/${encodeURIComponent(projectId)}`;
    entries.push(
      { label: "Features", icon: "✦", href: `${base}/features` },
      { label: "Orchestrations", icon: "⇄", href: `${base}/orchestrations` },
      { label: "Reviews", icon: "✔", href: `${base}/reviews` },
    );
  }
  entries.push({ label: "Settings", icon: "⚙", href: "#/settings" });
  return entries;
}

export function Shell({ projectId = null, children }) {
  const hash = useSyncExternalStore(subscribeToHash, currentHash, () => "");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const menuButton = useRef(null);
  const panel = useRef(null);
  const closeButton = useRef(null);
  const wasOpen = useRef(false);

  const entries = navigationEntries(projectId);

  useEffect(() => {
    if (!drawerOpen) {
      if (wasOpen.current) menuButton.current?.focus();
      wasOpen.current = false;
      return undefined;
    }
    wasOpen.current = true;
    const before = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    closeButton.current?.focus();
    const onKey = (event) => {
      if (event.key === "Escape") {
        setDrawerOpen(false);
        return;
      }
      if (event.key !== "Tab" || !panel.current) return;
      const items = [...panel.current.querySelectorAll("a[href], button")];
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const here = document.activeElement;
      if (event.shiftKey && (here === first || !panel.current.contains(here))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (here === last || !panel.current.contains(here))) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = before;
    };
  }, [drawerOpen]);

  const classes = ["shell", collapsed ? "collapsed" : "", drawerOpen ? "drawer-open" : ""].filter(Boolean).join(" ");

  return (
    <div className={classes} data-shell>
      <header className="topbar">
        <button type="button" ref={menuButton} data-menu-button aria-expanded={drawerOpen ? "true" : "false"} aria-controls="sidebar" aria-label="Menu" onClick={() => setDrawerOpen(true)}>
          ☰
        </button>
        <button type="button" className="rail-button" data-rail-button aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-pressed={collapsed ? "true" : "false"} onClick={() => setCollapsed(!collapsed)}>
          {collapsed ? "»" : "«"}
        </button>
      </header>
      {drawerOpen ? <div className="scrim" data-scrim onClick={() => setDrawerOpen(false)} /> : null}
      <aside id="sidebar" className="sidebar" ref={panel} role={drawerOpen ? "dialog" : undefined} aria-modal={drawerOpen ? "true" : undefined} aria-label="Navigation">
        <button type="button" ref={closeButton} className="close-button" data-close-button aria-label="Close menu" onClick={() => setDrawerOpen(false)}>
          ✕
        </button>
        <nav>
          {entries.map((entry) => (
            <a key={entry.href} href={entry.href} aria-current={hash === entry.href ? "page" : undefined} title={entry.label}>
              <span aria-hidden="true">{entry.icon}</span>
              <span className="label">{entry.label}</span>
            </a>
          ))}
        </nav>
      </aside>
      <div className="content">{children}</div>
    </div>
  );
}
