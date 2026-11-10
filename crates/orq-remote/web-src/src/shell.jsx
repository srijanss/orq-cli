// The page frame. On a desktop: a sidebar with the Dashboard, the chosen project's sections and
// Settings, which collapses to an icon rail. On a phone there is no sidebar: a top bar (theme,
// brand, sign out) and a fixed bottom tab bar hold the same destinations. The place lives in the
// address path, so Back and Forward move it.
import { Fragment, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Icon } from "./icons.jsx";
import { ThemeToggle } from "./theme.jsx";
import { currentPath, isPlainLeftClick, navigate, subscribeToLocation } from "./router.js";

// The same breakpoint the stylesheet uses for the phone layout.
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

function subscribeToPhone(notify) {
  if (typeof window.matchMedia !== "function") return () => {};
  const query = window.matchMedia(PHONE_QUERY);
  query.addEventListener("change", notify);
  return () => query.removeEventListener("change", notify);
}

function isPhone() {
  return typeof window.matchMedia === "function" && window.matchMedia(PHONE_QUERY).matches;
}

// The place, its destinations and the sentence assistive technology hears after a move. The region
// starts empty and its text is set after each move, so the change is read.
function usePlace(projectId) {
  const path = useSyncExternalStore(subscribeToLocation, currentPath, () => "");
  const entries = navigationEntries(projectId);
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => setAnnouncement(describePage(path, entries)), [path, projectId]);
  return { path, entries, announcement };
}

function goTo(entry, event) {
  if (isPlainLeftClick(event)) {
    event.preventDefault();
    navigate(entry.href);
  }
}

function Announcer({ text }) {
  return <div className="sr-only" role="status" aria-live="polite" data-announcer>{text}</div>;
}

export function Shell(props) {
  const phone = useSyncExternalStore(subscribeToPhone, isPhone, () => false);
  return phone ? <PhoneShell {...props} /> : <DesktopShell {...props} />;
}

function PhoneShell({ projectId = null, onSignOut = null, children }) {
  const { path, entries, announcement } = usePlace(projectId);
  return (
    <div className="shell phone" data-shell>
      <header className="topbar">
        <ThemeToggle />
        <span className="brand"><Icon name="logo" size={24} />Orq</span>
        {onSignOut ? (
          <button type="button" className="sign-out" data-sign-out aria-label="Sign out" title="Sign out" onClick={onSignOut}>
            <Icon name="logout" />
          </button>
        ) : <span />}
      </header>
      <main className="content">{children}</main>
      <nav className="tabbar" aria-label="Sections">
        {entries.map((entry) => (
          <a key={entry.href} href={entry.href} aria-current={path === entry.href ? "page" : undefined} onClick={(event) => goTo(entry, event)}>
            <Icon name={entry.icon} size={28} />
            <span className="tab-label">{entry.label}</span>
          </a>
        ))}
      </nav>
      <Announcer text={announcement} />
    </div>
  );
}

function DesktopShell({ projectId = null, projects = null, onOpenSwitcher = null, onSignOut = null, children }) {
  const { path, entries, announcement } = usePlace(projectId);
  const [collapsed, setCollapsed] = useState(false);

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

  return (
    <div className={collapsed ? "shell collapsed" : "shell"} data-shell>
      <aside id="sidebar" className="sidebar" aria-label="Navigation">
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
              <a href={entry.href} aria-current={path === entry.href ? "page" : undefined} title={entry.label} onClick={(event) => goTo(entry, event)}>
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
      <Announcer text={announcement} />
      <main className="content">{children}</main>
    </div>
  );
}
