// The page frame: a sidebar with the Dashboard, the chosen project's sections and Settings.
// The place lives in the address path, so Back and Forward move it. On a phone the sidebar is a
// drawer that behaves like a dialog (Escape, scrim or close button close it, focus is trapped and
// returned, page scroll is locked); on a desktop the same sidebar collapses to an icon rail.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Icon } from "./icons.jsx";
import { ThemeToggle } from "./theme.jsx";
import { currentPath, isPlainLeftClick, navigate, subscribeToLocation } from "./router.js";

// The same breakpoint the stylesheet uses for the drawer.
const PHONE_QUERY = "(max-width: 700px)";

export function navigationEntries(projectId) {
  const entries = [{ label: "Dashboard", icon: "home", href: "/dashboard" }];
  if (typeof projectId === "string" && projectId !== "") {
    const base = `/projects/${encodeURIComponent(projectId)}`;
    entries.push(
      { label: "Features", icon: "flag", href: `${base}/features` },
      { label: "Orchestrations", icon: "play", href: `${base}/orchestrations` },
      { label: "Reviews", icon: "doc", href: `${base}/reviews` },
    );
  }
  entries.push({ label: "Settings", icon: "gear", href: "/settings" });
  return entries;
}

export function Shell({ projectId = null, children }) {
  const path = useSyncExternalStore(subscribeToLocation, currentPath, () => "");
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
      const items = [...panel.current.querySelectorAll("a[href], button:not([data-rail-button])")];
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

  // Leaving the phone width turns the drawer back into the plain sidebar, so it must not stay modal.
  useEffect(() => {
    if (!drawerOpen || typeof window.matchMedia !== "function") return undefined;
    const query = window.matchMedia(PHONE_QUERY);
    const onChange = (event) => {
      if (!event.matches) setDrawerOpen(false);
    };
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, [drawerOpen]);

  const classes = ["shell", collapsed ? "collapsed" : "", drawerOpen ? "drawer-open" : ""].filter(Boolean).join(" ");

  return (
    <div className={classes} data-shell>
      <header className="topbar">
        <button type="button" ref={menuButton} data-menu-button aria-expanded={drawerOpen ? "true" : "false"} aria-controls="sidebar" aria-label="Menu" onClick={() => setDrawerOpen(true)}>
          <Icon name="menu" />
        </button>
        <span className="brand"><Icon name="logo" size={24} />Orq</span>
      </header>
      {drawerOpen ? <div className="scrim" data-scrim onClick={() => setDrawerOpen(false)} /> : null}
      <aside id="sidebar" className="sidebar" ref={panel} role={drawerOpen ? "dialog" : undefined} aria-modal={drawerOpen ? "true" : undefined} aria-label="Navigation">
        <button type="button" ref={closeButton} className="close-button" data-close-button aria-label="Close menu" onClick={() => setDrawerOpen(false)}>
          <Icon name="close" />
        </button>
        <div className="sidebar-head">
          <span className="brand" data-brand>
            <Icon name="logo" size={24} />
            <span className="label">Orq</span>
          </span>
          <button type="button" className="rail-button" data-rail-button aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-pressed={collapsed ? "true" : "false"} onClick={() => setCollapsed(!collapsed)}>
            <Icon name={collapsed ? "chevronsRight" : "chevronsLeft"} />
          </button>
        </div>
        <nav>
          {entries.map((entry) => (
            <a key={entry.href} href={entry.href} aria-current={path === entry.href ? "page" : undefined} title={entry.label} onClick={(event) => {
                if (isPlainLeftClick(event)) {
                  event.preventDefault();
                  navigate(entry.href);
                }
                setDrawerOpen(false);
              }}
            >
              <Icon name={entry.icon} />
              <span className="label">{entry.label}</span>
            </a>
          ))}
        </nav>
        <div className="sidebar-foot">
          <ThemeToggle />
        </div>
      </aside>
      <div className="content">{children}</div>
    </div>
  );
}
