// The page frame. On a desktop: a sidebar with the Dashboard, the chosen project's sections and
// Settings, which collapses to an icon rail. On a phone there is no sidebar: a top bar (theme,
// brand, the page's Refresh) and a fixed bottom tab bar of icon tabs hold the same destinations, its
// second row behind a grab handle. The place lives in the address path, so Back and Forward move it.
import { Fragment, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { Icon } from "./icons.jsx";
import { ThemeToggle } from "./theme.jsx";
import { currentPath, isPlainLeftClick, navigate, subscribeToLocation } from "./router.js";

// The same breakpoint the stylesheet uses for the phone layout.
const PHONE_QUERY = "(max-width: 700px)";

// An opened dropdown list scrolls itself, not the page, so its chosen row is fully in view.
export function showChosenRow(list) {
  if (list === null) return;
  const row = list.querySelector("[aria-selected=\"true\"]");
  if (row === null) return;
  const box = list.getBoundingClientRect();
  const top = box.top + list.clientTop;
  const bottom = top + list.clientHeight;
  const at = row.getBoundingClientRect();
  if (at.bottom > bottom) list.scrollTop += at.bottom - bottom;
  else if (at.top < top) list.scrollTop -= top - at.top;
}

export function navigationEntries(projectId) {
  const entries = [
    { label: "Dashboard", icon: "home", href: "/dashboard" },
    { label: "Todos", icon: "check", href: "/todos" },
  ];
  const base = typeof projectId === "string" && projectId !== "" ? `/projects/${encodeURIComponent(projectId)}` : "";
  entries.push(
    { label: "Features", icon: "flag", href: `${base}/features` },
    { label: "Orchestrations", icon: "play", href: `${base}/orchestrations` },
    { label: "Queue", icon: "clock", href: `${base}/queue` },
    { label: "Reviews", icon: "doc", href: `${base}/reviews` },
    { label: "Stats", icon: "chart", href: `${base}/stats` },
  );
  entries.push({ label: "Settings", icon: "gear", href: "/settings" });
  return entries;
}

// A section stays marked on the pages under it, like a run under Orchestrations or the audit under Settings.
function within(path, href) {
  return path === href || path.startsWith(href + "/");
}

const SECTIONS = { features: "Features", orchestrations: "Orchestrations", queue: "Queue", reviews: "Reviews", stats: "Stats" };

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

// "Ask orq": a toggle among the sections, not a link. It opens and closes the chat panel over or beside the
// page, which stays where it is; while the panel is open it has the section pill too. `ask` is
// { open, onToggle } from the page, or null where nothing listens (the entry then does nothing).
function AskToggle({ ask, className = undefined, size = 20, label = "label", tabIndex = undefined, onToggled = undefined }) {
  const open = ask !== null && ask.open === true;
  return (
    <button type="button" className={className} data-ask-orq tabIndex={tabIndex} aria-expanded={open ? "true" : "false"} title="Ask orq" onClick={() => {
        ask?.onToggle?.();
        onToggled?.(!open);
      }}>
      <Icon name="sparkle" size={size} />
      <span className={label}>Ask orq</span>
    </button>
  );
}

// Signing out on a phone is on the Settings page; the bar's corner holds the page's Refresh.
function PhoneShell({ projectId = null, ask = null, panel = null, children }) {
  const { path, entries, announcement } = usePlace(projectId);
  const refresh = useSyncExternalStore(subscribeToRefresh, () => pageRefresh);
  return (
    <div className={ask !== null && ask.open ? "shell phone asking" : "shell phone"} data-shell>
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
      <TabBar entries={entries} path={path} ask={ask} />
      {ask !== null && ask.open ? panel : null}
      <Announcer text={announcement} />
    </div>
  );
}

// A finger has to move this far before a press on the handle is a drag rather than a tap.
const DRAG_SLOP = 6;
// A click within this long of a drag's end is the drag's own, not a tap.
const DRAG_CLICK_MS = 300;
const SETTLE = "height 200ms ease-out";
const now = () => performance.now();

function prefersReducedMotion() {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

// Whether the second row was left open, per browser; closed when storage is unavailable.
const MORE_KEY = "orq.tabbar.more";

function storedMoreOpen() {
  try {
    return window.localStorage.getItem(MORE_KEY) === "open";
  } catch (unavailable) {
    return false;
  }
}

function storeMoreOpen(open) {
  try {
    window.localStorage.setItem(MORE_KEY, open ? "open" : "closed");
  } catch (unavailable) {
    // Without storage it is only forgotten on the next visit.
  }
}

// Todos, Reviews and Stats sit in the bar's second row; every other section is in the first.
function inSecondRow(entry) {
  return entry.href === "/todos" || entry.href.endsWith("/reviews") || entry.href.endsWith("/stats");
}

// The bar's icon tabs, on five columns: Home, Features, Orchestrations, Queue and Settings always show,
// under a short grip handle; Todos, Reviews, Stats and Ask orq are a second row the handle slides in and out.
// Each name stays as a visually hidden label.
export function TabBar({ entries, path, ask = null }) {
  const first = entries.filter((entry) => !inSecondRow(entry));
  const second = entries.filter(inSecondRow);
  // On a page whose section is in the second row, or with the Ask orq panel open, the row opens so the
  // marked tab is in sight: when the bar mounts, and when such a page or the panel comes up later, except
  // for a page chosen from the row itself, which closes it.
  const askOpen = ask !== null && ask.open === true;
  const onSecondRow = second.some((entry) => within(path, entry.href));
  const [open, setShown] = useState(() => onSecondRow || askOpen || storedMoreOpen());
  // What the person opens or closes is remembered; what opens by itself is not.
  const setOpen = (value) => {
    setShown(value);
    storeMoreOpen(value);
  };
  const [seen, setSeen] = useState({ path, askOpen });
  const chosen = useRef(false);
  if (seen.path !== path || seen.askOpen !== askOpen) {
    setSeen({ path, askOpen });
    const cameTo = seen.path !== path && onSecondRow && !chosen.current;
    if (cameTo || (askOpen && !seen.askOpen)) setShown(true);
  }
  useEffect(() => {
    chosen.current = false;
  }, [path]);
  const choose = (entry) => {
    // Choosing the page already shown moves nowhere, so nothing is left to skip.
    chosen.current = entry.href !== path;
    setOpen(false);
  };
  const row = useRef(null);
  const bar = useRef(null);
  // The page's content clears the bar at whatever height it has, open or closed.
  useLayoutEffect(() => {
    const nav = bar.current;
    const root = document.documentElement;
    if (!nav || typeof window.ResizeObserver !== "function") return undefined;
    const observer = new window.ResizeObserver(() => root.style.setProperty("--tabbar-height", `${Math.ceil(nav.getBoundingClientRect().height)}px`));
    observer.observe(nav);
    return () => {
      observer.disconnect();
      root.style.removeProperty("--tabbar-height");
    };
  }, []);
  // The row's open height, measured when it opens, and how much of it shows while a finger drags.
  const [full, setFull] = useState(0);
  const [pull, setPull] = useState(null);
  const drag = useRef(null);
  const dragEndedAt = useRef(-Infinity);
  const reduced = prefersReducedMotion();
  const measure = () => row.current?.scrollHeight ?? 0;
  useLayoutEffect(() => {
    if (open) setFull(measure());
  }, [open]);

  // How much of the row shows with the finger at `y`.
  const showing = (current, y) => Math.min(current.height, Math.max(0, current.from + current.startY - y));
  const onPointerDown = (event) => {
    const height = open ? full : measure();
    drag.current = { startY: event.clientY, from: open ? height : 0, height, moved: false };
  };
  const onPointerMove = (event) => {
    const current = drag.current;
    if (!current) return;
    if (!current.moved && Math.abs(event.clientY - current.startY) < DRAG_SLOP) return;
    if (!current.moved) event.currentTarget.setPointerCapture?.(event.pointerId);
    current.moved = true;
    if (!reduced) setPull(showing(current, event.clientY));
  };
  // Let go, the row settles open past half way and closed short of it.
  const onPointerUp = (event) => {
    const current = drag.current;
    drag.current = null;
    if (!current || !current.moved) return;
    dragEndedAt.current = now();
    setPull(null);
    const opens = showing(current, event.clientY) > current.height / 2;
    if (opens) setFull(current.height);
    setOpen(opens);
  };
  // A cancelled drag, or one whose finger the handle lost, falls back to where it started.
  const onPointerCancel = () => {
    if (!drag.current) return;
    drag.current = null;
    setPull(null);
  };
  const onLostPointerCapture = (event) => {
    if (event.target === event.currentTarget) onPointerCancel();
  };

  const height = pull !== null ? pull : open ? full : 0;
  const transition = pull !== null || reduced ? "none" : SETTLE;
  const away = open ? undefined : -1;
  return (
    <nav className="tabbar" aria-label="Sections" ref={bar} data-open={open ? "" : undefined}>
      <button type="button" className="tabbar-handle" data-tab-handle aria-label="More sections" aria-expanded={open ? "true" : "false"} aria-controls="tabbar-more"
        onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel} onLostPointerCapture={onLostPointerCapture}
        onClick={() => {
          // The click a browser sends after a drag's lift is the drag's own, not a tap.
          if (now() - dragEndedAt.current < DRAG_CLICK_MS) return;
          setOpen(!open);
        }} />
      <div className="tabbar-row" data-tab-row="1">
        {first.map((entry) => (
          <Tab key={entry.href} entry={entry} path={path} />
        ))}
      </div>
      <div id="tabbar-more" className="tabbar-row tabbar-more" data-tab-row="2" data-tab-drawer ref={row} aria-hidden={open ? undefined : "true"} style={{ height: `${height}px`, transition }}>
        {second.map((entry) => (
          <Tab key={entry.href} entry={entry} path={path} tabIndex={away} onChoose={choose} />
        ))}
        <AskToggle ask={ask} size={28} label="tab-label" tabIndex={away} onToggled={(opened) => {
            if (!opened) setOpen(false);
          }} />
      </div>
    </nav>
  );
}

function Tab({ entry, path, tabIndex = undefined, onChoose = undefined }) {
  return (
    <a href={entry.href} title={entry.label} tabIndex={tabIndex} aria-current={within(path, entry.href) ? "page" : undefined} onClick={(event) => {
        if (isPlainLeftClick(event)) onChoose?.(entry);
        goTo(entry, event);
      }}>
      <Icon name={entry.icon} size={28} />
      <span className="tab-label">{entry.label}</span>
    </a>
  );
}

function DesktopShell({ projectId = null, projects = null, onOpenSwitcher = null, onSignOut = null, ask = null, panel = null, children }) {
  const { path, entries, announcement } = usePlace(projectId);
  const [collapsed, setCollapsed] = useState(false);

  const [listOpen, setListOpen] = useState(false);
  const switchBox = useRef(null);
  const switchList = useRef(null);
  useEffect(() => {
    if (listOpen) showChosenRow(switchList.current);
  }, [listOpen]);
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
    <div className={"shell" + (collapsed ? " collapsed" : "") + (ask !== null && ask.open ? " asking" : "")} data-shell>
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
                  <ul className="project-list dropdown-list" ref={switchList} data-project-list role="listbox" aria-label="Project" hidden={!listOpen}>
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
              {entry.href.endsWith("/reviews") ? <AskToggle ask={ask} className="nav-toggle" /> : null}
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
      {ask !== null && ask.open ? panel : null}
    </div>
  );
}
