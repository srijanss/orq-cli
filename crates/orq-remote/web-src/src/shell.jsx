// The page frame. On a desktop: a sidebar with the Dashboard, the chosen project's sections and
// Settings, which collapses to an icon rail. On a phone there is no sidebar: a top bar (theme,
// brand, the page's Refresh) and a fixed bottom tab bar hold the same destinations; past five, the bar
// keeps five and a grab handle on its top edge pulls the bar up to show them all. The place lives in the
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

// The bar holds five tabs; past five, a grab handle on its top edge pulls the bar up to show every section.
const BAR_TABS = 5;
// A finger has to move this far before a press on the handle is a drag rather than a tap.
const DRAG_SLOP = 6;
// Letting go faster than this (px per ms, over the last 100ms) flicks the bar open or closed.
const FLICK_SPEED = 0.5;
const FLICK_WINDOW = 100;
// A click within this long of a drag's end is the drag's own, not a tap.
const DRAG_CLICK_MS = 300;
const SETTLE = "height 200ms ease-out";

function prefersReducedMotion() {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

const now = () => performance.now();

export function TabBar({ entries, path }) {
  // The drawer is open on the page it was opened on, so any move, Back and Forward included,
  // closes it; the move also forgets it, so coming back doesn't reopen it.
  const [openOn, setOpenOn] = useState(null);
  if (openOn !== null && openOn !== path) setOpenOn(null);
  const open = openOn === path;
  // The drawer's open height, measured when it opens, and how much of it shows while a finger drags.
  const [full, setFull] = useState(0);
  const [pull, setPull] = useState(null);
  const overflows = entries.length > BAR_TABS;
  const shown = entries.slice(0, BAR_TABS);
  const reduced = prefersReducedMotion();
  const bar = useRef(null);
  const handle = useRef(null);
  const drawer = useRef(null);
  const drag = useRef(null);
  const dragEndedAt = useRef(-Infinity);
  const wasOpen = useRef(false);

  const measure = () => drawer.current?.scrollHeight ?? 0;
  const setOpen = (value) => {
    if (value) setFull(measure());
    setOpenOn(value ? path : null);
  };

  useEffect(() => {
    if (!open) {
      // Focus goes back to the handle however the drawer closed.
      if (wasOpen.current) handle.current?.focus();
      wasOpen.current = false;
      return undefined;
    }
    wasOpen.current = true;
    const tiles = [...(drawer.current?.querySelectorAll("a") ?? [])];
    (tiles.find((tile) => tile.getAttribute("aria-current") === "page") ?? tiles[0])?.focus();
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        setOpenOn(null);
        return;
      }
      if (event.key !== "Tab" || !bar.current) return;
      // Tab and Shift+Tab go round the handle, the tiles and the tabs while the drawer is open.
      const stops = [...bar.current.querySelectorAll("a[href], button")].filter((stop) => stop.tabIndex >= 0);
      if (stops.length === 0) return;
      event.preventDefault();
      const at = stops.indexOf(document.activeElement);
      const next = event.shiftKey ? (at <= 0 ? stops.length - 1 : at - 1) : (at === -1 || at === stops.length - 1 ? 0 : at + 1);
      stops[next].focus();
    };
    // A resize can reflow the grid (five a row from 600px wide, four below), so the open height follows it.
    const onResize = () => setFull(measure());
    document.addEventListener("keydown", onKeyDown);
    window.addEventListener("resize", onResize);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("resize", onResize);
    };
  }, [open]);

  // How much of the drawer shows with the finger at `y`.
  const showing = (current, y) => Math.min(current.height, Math.max(0, current.from + current.startY - y));

  const onPointerDown = (event) => {
    if (!overflows) return;
    // The handle, or the bar's own empty strip; a press on a tab or tile stays a tap.
    if (!handle.current?.contains(event.target) && event.target !== bar.current) return;
    const height = open ? full : measure();
    drag.current = { startY: event.clientY, from: open ? height : 0, height, moved: false, samples: [{ y: event.clientY, t: now() }] };
  };

  const onPointerMove = (event) => {
    const current = drag.current;
    if (!current) return;
    if (!current.moved && Math.abs(event.clientY - current.startY) < DRAG_SLOP) return;
    // The bar takes the finger only once it drags: a capture from the press would send a tap's click
    // to the bar instead of the handle.
    if (!current.moved) event.currentTarget.setPointerCapture?.(event.pointerId);
    current.moved = true;
    const t = now();
    current.samples = current.samples.filter((sample) => sample.t >= t - FLICK_WINDOW).concat({ y: event.clientY, t });
    if (!reduced) setPull(showing(current, event.clientY));
  };

  const onPointerUp = (event) => {
    const current = drag.current;
    drag.current = null;
    if (!current || !current.moved) return;
    dragEndedAt.current = now();
    setPull(null);
    // Upward speed over the last moments of the drag; 0 when the finger rested before lifting.
    const t = now();
    const recent = current.samples.find((sample) => sample.t >= t - FLICK_WINDOW);
    const speed = recent ? (recent.y - event.clientY) / Math.max(1, t - recent.t) : 0;
    const visible = showing(current, event.clientY);
    const opens = current.from === 0
      ? visible > current.height / 3 || speed > FLICK_SPEED
      : !(current.height - visible > current.height / 3 || -speed > FLICK_SPEED);
    if (opens) setFull(current.height);
    setOpenOn(opens ? path : null);
  };

  // A cancelled drag, or one whose finger the bar lost (the lift releases it too, after the drag has ended),
  // falls back to where it started.
  const onPointerCancel = () => {
    if (!drag.current) return;
    drag.current = null;
    setPull(null);
  };

  const height = pull !== null ? pull : open ? full : 0;
  const transition = pull !== null || reduced ? "none" : SETTLE;

  return (
    <>
      {open ? <div className="tabbar-backdrop" data-tab-backdrop aria-hidden="true" onClick={() => setOpenOn(null)} /> : null}
      <nav className="tabbar" aria-label="Sections" ref={bar} data-open={open ? "" : undefined} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel} onLostPointerCapture={onPointerCancel}>
        {overflows ? (
          <>
            <button type="button" ref={handle} className="tabbar-handle" data-tab-handle aria-label="Show all sections" aria-expanded={open ? "true" : "false"} aria-controls="tabbar-all" onClick={() => {
                if (now() - dragEndedAt.current < DRAG_CLICK_MS) return;
                setOpen(!open);
              }} />
            <div id="tabbar-all" className="tabbar-all" data-tab-drawer ref={drawer} aria-hidden={open ? undefined : "true"} style={{ height: `${height}px`, transition }}>
              <div className="tabbar-grid">
                {entries.map((entry) => (
                  <a key={entry.href} href={entry.href} tabIndex={open ? undefined : -1} aria-current={within(path, entry.href) ? "page" : undefined} onClick={(event) => {
                      setOpenOn(null);
                      goTo(entry, event);
                    }}>
                    <Icon name={entry.icon} size={28} />
                    <span className="tile-label">{entry.label}</span>
                  </a>
                ))}
              </div>
            </div>
          </>
        ) : null}
        {shown.map((entry) => (
          <a key={entry.href} href={entry.href} aria-current={within(path, entry.href) ? "page" : undefined} onClick={(event) => goTo(entry, event)}>
            <Icon name={entry.icon} size={28} />
            <span className="tab-label">{entry.label}</span>
          </a>
        ))}
      </nav>
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
