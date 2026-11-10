// The page frame: a sidebar with the Dashboard, the chosen project's sections and Settings.
// The place lives in the address path, so Back and Forward move it. On a phone the sidebar is a
// drawer that behaves like a dialog (Escape, scrim or close button close it, focus is trapped and
// returned, page scroll is locked); on a desktop the same sidebar collapses to an icon rail.
import { Fragment, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Icon } from "./icons.jsx";
import { ThemeToggle } from "./theme.jsx";
import { currentPath, isPlainLeftClick, navigate, subscribeToLocation } from "./router.js";

// The same breakpoint the stylesheet uses for the drawer.
const PHONE_QUERY = "(max-width: 700px)";

export function navigationEntries(projectId) {
  const entries = [{ label: "Dashboard", icon: "home", href: "/dashboard" }];
  const base = typeof projectId === "string" && projectId !== "" ? `/projects/${encodeURIComponent(projectId)}` : "";
  entries.push(
    { label: "Features", icon: "flag", href: `${base}/features` },
    { label: "Orchestrations", icon: "play", href: `${base}/orchestrations` },
    { label: "Reviews", icon: "doc", href: `${base}/reviews` },
  );
  entries.push({ label: "Settings", icon: "gear", href: "/settings" });
  return entries;
}

const SECTIONS = { features: "Features", orchestrations: "Orchestrations", reviews: "Reviews" };

// What assistive technology hears after moving to `path`; "" for a log page.
function describePage(path, entries) {
  if (path.includes("/logs")) return "";
  const parts = path.split("/");
  const decode = (text) => {
    try {
      return decodeURIComponent(text);
    } catch (bad) {
      return text;
    }
  };
  if (parts[1] === "projects" && SECTIONS[parts[3]] !== undefined) {
    const detail = parts[4] ? ": " + decode(parts[4]) : "";
    return SECTIONS[parts[3]] + detail + ", project " + decode(parts[2]);
  }
  if (parts[1] === "reviews" && parts[2]) return "Review report: " + decode(parts[2]);
  const entry = entries.find((candidate) => candidate.href === path);
  return entry !== undefined ? entry.label + " page" : "";
}

export function Shell({ projectId = null, projects = null, onOpenSwitcher = null, onSignOut = null, children }) {
  const path = useSyncExternalStore(subscribeToLocation, currentPath, () => "");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const menuButton = useRef(null);
  const panel = useRef(null);
  const closeButton = useRef(null);
  const wasOpen = useRef(false);

  const entries = navigationEntries(projectId);

  const [listOpen, setListOpen] = useState(false);
  const switchBox = useRef(null);
  const choices = [{ value: "", label: "All projects" }];
  if (projectId !== null && !(projects ?? []).some((project) => project.id === projectId)) choices.push({ value: projectId, label: projectId });
  for (const project of projects ?? []) choices.push({ value: project.id, label: project.name });
  const currentName = choices.find((choice) => choice.value === (projectId ?? ""))?.label ?? "All projects";

  const choose = (chosen) => {
    const parts = path.split("/");
    const section = projectId !== null ? parts[3] : parts[1];
    if (chosen === "") navigate(SECTIONS[section] !== undefined ? "/" + section : "/dashboard");
    else navigate("/projects/" + encodeURIComponent(chosen) + "/" + (projectId !== null ? section || "reviews" : SECTIONS[section] !== undefined ? section : "reviews"));
    setListOpen(false);
    setDrawerOpen(false);
  };

  // Escape or a press outside the chooser closes the open list.
  useEffect(() => {
    if (!listOpen) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") setListOpen(false);
    };
    const onPress = (event) => {
      if (switchBox.current && !switchBox.current.contains(event.target)) setListOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPress);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPress);
    };
  }, [listOpen]);

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
      const items = [...panel.current.querySelectorAll("a[href], button:not([data-rail-button]):not([data-project-option])")];
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

  // A page change is announced by name, with the project and the run or report when there is one.
  // The region starts empty and its text is set after each move, so assistive technology reads the
  // change. Log pages stay out so a streaming log never talks.
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => setAnnouncement(describePage(path, entries)), [path, projectId]);
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
        <nav aria-label="Primary">
          {entries.map((entry) => (
            <Fragment key={entry.href}>
              {entry.href === "/settings" ? <div className="sidebar-divider" role="separator" /> : null}
              <a href={entry.href} aria-current={path === entry.href ? "page" : undefined} title={entry.label} onClick={(event) => {
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
              {entry.href === "/dashboard" ? (
                <div className="project-switch" ref={switchBox}>
                  <span className="project-switch-prefix">Project:</span>
                  <button type="button" className="project-switcher" data-project-switcher data-value={projectId ?? ""} aria-label="Project" aria-haspopup="listbox" aria-expanded={listOpen ? "true" : "false"} onClick={() => {
                      if (!listOpen) onOpenSwitcher?.();
                      setListOpen(!listOpen);
                    }}>
                    <span className="project-switcher-text">{currentName}</span>
                    <Icon name="chevronDown" size={18} />
                  </button>
                  <ul className="project-list" data-project-list role="listbox" aria-label="Project" hidden={!listOpen}>
                    {choices.map((choice) => (
                      <li key={choice.value} role="presentation">
                        <button type="button" role="option" tabIndex={-1} data-project-option={choice.value} aria-selected={choice.value === (projectId ?? "") ? "true" : "false"} onClick={() => choose(choice.value)}>
                          {choice.label}
                        </button>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </Fragment>
          ))}
        </nav>
        <div className="sidebar-foot">
          <ThemeToggle />
          <div className="sidebar-divider" role="separator" />
          {onSignOut ? (
            <button type="button" className="sign-out" data-sign-out aria-label="Sign out" title="Sign out" onClick={onSignOut}>
              <Icon name="logout" />
              <span className="label">Sign out</span>
            </button>
          ) : null}
        </div>
      </aside>
      <nav className="tabbar" aria-label="Sections">
        {entries.map((entry) => (
          <a key={entry.href} href={entry.href} aria-current={path === entry.href ? "page" : undefined} onClick={(event) => {
              if (isPlainLeftClick(event)) {
                event.preventDefault();
                navigate(entry.href);
              }
            }}
          >
            <Icon name={entry.icon} />
            <span className="tab-label">{entry.label}</span>
          </a>
        ))}
      </nav>
      <div className="sr-only" role="status" aria-live="polite" data-announcer>{announcement}</div>
      <main className="content">{children}</main>
    </div>
  );
}
