// The page frame. On a desktop: a sidebar with the Dashboard, the chosen project's sections and
// Settings, which collapses to an icon rail. On a phone there is no sidebar: a top bar (theme,
// brand, the page's Refresh) and a fixed bottom tab bar hold the same destinations; past five, the bar
// keeps four and a More tab opens a sheet with the rest. The place lives in the
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
    { label: "Queue", icon: "clock", href: `${base}/queue` },
    { label: "Reviews", icon: "doc", href: `${base}/reviews` },
  );
  entries.push({ label: "Settings", icon: "gear", href: "/settings" });
  return entries;
}

// A section stays marked on the pages under it, like a run under Orchestrations or the audit under Settings.
function within(path, href) {
  return path === href || path.startsWith(href + "/");
}

const SECTIONS = { features: "Features", orchestrations: "Orchestrations", queue: "Queue", reviews: "Reviews" };

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

export function usePhone() {
  return useSyncExternalStore(subscribeToPhone, isPhone, () => false);
}

// The open page's Refresh, which a phone's top bar shows (the page header then has none). A page
// registers it while it is shown; the bar always calls the page's latest one.
let pageRefresh = null;
const refreshListeners = new Set();

function setPageRefresh(refresh) {
  pageRefresh = refresh;
  for (const listener of [...refreshListeners]) listener();
}

function subscribeToRefresh(listener) {
  refreshListeners.add(listener);
  return () => refreshListeners.delete(listener);
}

export function usePageRefresh(onRefresh) {
  const latest = useRef(onRefresh);
  latest.current = onRefresh;
  const has = Boolean(onRefresh);
  useEffect(() => {
    if (!has) return undefined;
    const refresh = () => latest.current && latest.current();
    setPageRefresh(refresh);
    return () => {
      if (pageRefresh === refresh) setPageRefresh(null);
    };
  }, [has]);
}

export function Shell(props) {
  const phone = usePhone();
  return phone ? <PhoneShell {...props} /> : <DesktopShell {...props} />;
}

// Signing out on a phone is on the Settings page; the bar's corner holds the page's Refresh.
function PhoneShell({ projectId = null, children }) {
  const { path, entries, announcement } = usePlace(projectId);
  const refresh = useSyncExternalStore(subscribeToRefresh, () => pageRefresh);
  return (
    <div className="shell phone" data-shell>
      <header className="topbar">
        <ThemeToggle />
        <span className="brand"><Icon name="logo" size={24} />Orq</span>
        {refresh !== null ? (
          <button type="button" className="icon-button topbar-refresh" data-refresh aria-label="Refresh" title="Refresh" onClick={refresh}>
            <Icon name="refresh" />
          </button>
        ) : <span />}
      </header>
      <main className="content">{children}</main>
      <TabBar entries={entries} path={path} />
      <Announcer text={announcement} />
    </div>
  );
}

// The bar holds four tabs; the rest sit behind a More tab.
const BAR_TABS = 4;

export function TabBar({ entries, path }) {
  const [open, setOpen] = useState(false);
  // Five fit the bar as they are; a sixth turns the fifth place into More.
  const overflows = entries.length > BAR_TABS + 1;
  const shown = overflows ? entries.slice(0, BAR_TABS) : entries;
  const rest = overflows ? entries.slice(BAR_TABS) : [];
  const sheet = useRef(null);
  const more = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    sheet.current?.querySelector("a")?.focus();
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      setOpen(false);
      more.current?.focus();
    };
    // A press anywhere but the sheet or More (which toggles it itself) closes it.
    const onPress = (event) => {
      if (!sheet.current?.contains(event.target) && !more.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("click", onPress);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("click", onPress);
    };
  }, [open]);
  return (
    <>
      <nav className="tabbar" aria-label="Sections">
        {shown.map((entry) => (
          <a key={entry.href} href={entry.href} aria-current={within(path, entry.href) ? "page" : undefined} onClick={(event) => goTo(entry, event)}>
            <Icon name={entry.icon} size={28} />
            <span className="tab-label">{entry.label}</span>
          </a>
        ))}
        {overflows ? (
          <button type="button" ref={more} data-tab-more aria-haspopup="dialog" aria-expanded={open ? "true" : "false"} aria-current={rest.some((entry) => within(path, entry.href)) ? "page" : undefined} onClick={() => setOpen(!open)}>
            <Icon name="dots" size={28} />
            <span className="tab-label">More</span>
          </button>
        ) : null}
      </nav>
      {open ? (
        <div className="more-sheet" role="dialog" aria-label="More" data-more-sheet ref={sheet}>
          {rest.map((entry) => (
            <a key={entry.href} href={entry.href} aria-current={within(path, entry.href) ? "page" : undefined} onClick={(event) => {
                setOpen(false);
                goTo(entry, event);
              }}>
              <Icon name={entry.icon} />
              <span>{entry.label}</span>
            </a>
          ))}
        </div>
      ) : null}
    </>
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
              <a href={entry.href} aria-current={within(path, entry.href) ? "page" : undefined} title={entry.label} onClick={(event) => goTo(entry, event)}>
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
