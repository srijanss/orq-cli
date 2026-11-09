// The review inbox page. The bearer token is typed in, held in one piece of state and sent as a
// header on each request. A copy is kept in this tab's sessionStorage only, so a refresh does not
// ask again and closing the tab forgets it; it goes nowhere else (no address, no other storage).
// Every string from the API is rendered as text.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { Shell } from "./shell.jsx";
import { Badge } from "./badge.jsx";
import { Icon } from "./icons.jsx";
import { ThemeToggle } from "./theme.jsx";
import { authStatus, listPasskeys, passkeyBlocker, passkeysAvailable, registerPasskey, signInWithPasskey, signOutOfSession } from "./passkey.js";
import { currentPath, isPlainLeftClick, migrateLegacyHash, navigate, subscribeToLocation } from "./router.js";

const API_REVIEWS = "/api/reviews";
// One message for every failed sign-in, so it never says which part failed.
const SIGN_IN_FAILED = "Could not sign in. Check the token and try again.";
const STALE_MARKER = "stale, may be out of date";
const UNVERIFIED_MARKER = "unverified, could not be checked";
const DETAIL_PREFIX = "/reviews/";
// Places the sidebar offers that have no page yet; they say so instead of showing the inbox.
const UNBUILT_DESTINATIONS = ["/projects"];
const API_SETTINGS = "/api/settings";
const API_PROJECTS = "/api/projects";
// The same plain identifiers the server accepts, checked before one is put in a
// path or an address.
const RUN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const DETAIL_SECTIONS = [
  ["Summary", "summary"],
  ["Risks", "risks"],
  ["Files", "files"],
  ["Tests", "tests"],
  ["Review rounds", "rounds"],
];

// The server sends the time as plain UTC text; anything else is not shown.
const UTC_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;

function validTime(at) {
  if (typeof at !== "string" || !UTC_TIME.test(at)) return null;
  const when = new Date(at);
  return Number.isNaN(when.getTime()) ? null : when;
}

// "5 minutes ago"; a time ahead of this clock counts as just now rather than as the future.
function relativeTime(when, now = Date.now()) {
  const seconds = Math.max(0, Math.floor((now - when.getTime()) / 1000));
  if (seconds < 60) return "just now";
  const units = [["day", 86400], ["hour", 3600], ["minute", 60]];
  const [name, size] = units.find(([, size]) => seconds >= size);
  const count = Math.floor(seconds / size);
  return count + " " + name + (count === 1 ? "" : "s") + " ago";
}

// One shared clock re-renders every mounted time, so a shown age keeps growing even when no reply
// arrives. It runs only while a time is on the page.
const CLOCK_MS = 20000;
const clockListeners = new Set();
let clockTimer = null;

function useClock() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const listener = () => setTick((n) => n + 1);
    clockListeners.add(listener);
    if (clockTimer === null) clockTimer = setInterval(() => clockListeners.forEach((run) => run()), CLOCK_MS);
    return () => {
      clockListeners.delete(listener);
      if (clockListeners.size === 0 && clockTimer !== null) {
        clearInterval(clockTimer);
        clockTimer = null;
      }
    };
  }, []);
}

// A time is shown relative to now, with the absolute local time in its title.
function Moment({ at }) {
  useClock();
  const when = validTime(at);
  if (when === null) return null;
  return (
    <time dateTime={at} title={when.toLocaleString([], { dateStyle: "medium", timeStyle: "medium" })}>
      {relativeTime(when)}
    </time>
  );
}

// A labelled time; nothing at all when the time is missing or odd.
function Stamp({ label, at }) {
  if (validTime(at) === null) return null;
  return (
    <p>
      {label} <Moment at={at} />
    </p>
  );
}

function Generated({ at }) {
  if (validTime(at) === null) return null;
  return (
    <span className="generated">
      Reviewed at <Moment at={at} />
    </span>
  );
}

function isRun(run) {
  return run !== null && typeof run === "object" && typeof run.feature === "string" && typeof run.status === "string";
}

function parseRuns(data) {
  return data && Array.isArray(data.reviews) && data.reviews.every(isRun) ? data.reviews : null;
}

function isTextList(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function parseDetail(data) {
  const ok = data !== null && typeof data === "object" && typeof data.stale === "boolean" && DETAIL_SECTIONS.every(([, key]) => isTextList(data[key]));
  return ok ? data : null;
}

// A provider link is offered only when it is a plain https address without
// credentials, and never for a stale report.
function safeHttpsLink(value) {
  if (typeof value !== "string" || !value.startsWith("https://")) {
    return null;
  }
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.username === "" && url.password === "" && url.hostname !== "" ? url.href : null;
  } catch (invalid) {
    return null;
  }
}

function PasskeyButton({ onSignedIn, enrolled = true, onAskToken }) {
  const [failed, setFailed] = useState(false);
  const go = async () => {
    if (!enrolled) {
      onAskToken();
      return;
    }
    setFailed(false);
    let ok = false;
    try {
      ok = await signInWithPasskey();
    } catch (cancelled) {
      ok = false;
    }
    if (ok) onSignedIn();
    else setFailed(true);
  };
  return (
    <div className="passkey-signin">
      <button type="button" onClick={go}>
        Sign in with a passkey
      </button>
      {failed && <p role="alert">Could not sign in with a passkey.</p>}
    </div>
  );
}

function TokenPrompt({ message, onToken, passkeys = false, known = false, onPasskey, blocker = null }) {
  const field = useRef(null);
  const [asking, setAsking] = useState(false);
  const usable = known && blocker === null && passkeysAvailable();
  const submit = () => {
    const typed = field.current.value.trim();
    field.current.value = "";
    if (typed !== "") {
      onToken(typed);
    }
  };
  return (
    <div className="signin card">
      <span className="brand">
        <Icon name="logo" size={28} />
        Orq
      </span>
      <h1>Sign in</h1>
      {usable && !asking && <PasskeyButton onSignedIn={onPasskey} enrolled={passkeys} onAskToken={() => setAsking(true)} />}
      {blocker !== null && <p role="status">{blocker}</p>}
      {(!usable || asking) && (
        <>
          <p>Sign in with token: paste the access token printed by orq serve.</p>
          <label>
            Access token
            <input ref={field} type="password" autoComplete="off" spellCheck={false} aria-label="Access token" onKeyDown={(event) => event.key === "Enter" && submit()} />
          </label>
          <button type="button" onClick={submit}>
            Open the inbox
          </button>
        </>
      )}
      <p role="alert">{message}</p>
      <div className="alternatives" />
      <ThemeToggle />
    </div>
  );
}

function RunRow({ run, detailPrefix, selected = false }) {
  const open = typeof run.run_id === "string" && RUN_ID.test(run.run_id);
  return (
    <li className={run.stale === true ? "run stale card" : "run card"} data-selected={selected ? run.run_id : undefined}>
      {open ? <span className="ticket-id">{run.run_id}</span> : null}
      {open ? (
        <button type="button" data-open-run={run.run_id} aria-current={selected ? "true" : undefined} onClick={() => navigate(detailPrefix + run.run_id)}>
          {String(run.feature)}
        </button>
      ) : (
        <strong>{String(run.feature)}</strong>
      )}
      <span>{String(run.status)}</span>
      {run.risk !== undefined && run.risk !== null && <span>{"risk: " + String(run.risk)}</span>}
      <Generated at={run.generated_at} />
      {rowFreshness(run) === "stale" && <em>{STALE_MARKER}</em>}
      {rowFreshness(run) === "unverified" && <em>{UNVERIFIED_MARKER}</em>}
    </li>
  );
}

// A row's freshness: the server's word when it gives one, else stale or current from its flag.
function rowFreshness(run) {
  if (run.freshness === "current" || run.freshness === "stale" || run.freshness === "unverified") return run.freshness;
  return run.stale === true ? "stale" : "current";
}

function reviewTime(run) {
  const when = typeof run.generated_at === "string" && UTC_TIME.test(run.generated_at) ? Date.parse(run.generated_at) : NaN;
  return Number.isNaN(when) ? -Infinity : when;
}

const REVIEW_FILTERS = [
  ["status", "Status", (run) => String(run.status)],
  ["risk", "Risk", (run) => (run.risk === undefined || run.risk === null ? "" : String(run.risk))],
  ["freshness", "Freshness", rowFreshness],
];

// Newest review first; the filters only hide rows, so a marker never leaves a row that is shown.
function RunList({ runs, detailPrefix = DETAIL_PREFIX, selected = null }) {
  const [chosen, setChosen] = useState({});
  if (runs.length === 0) {
    return <p>No reviews in the inbox.</p>;
  }
  const shown = runs
    .filter((run) => REVIEW_FILTERS.every(([key, , read]) => !chosen[key] || read(run) === chosen[key]))
    .sort((a, b) => reviewTime(b) - reviewTime(a));
  return (
    <>
      <div className="filters" role="group" aria-label="Filter reviews">
        {REVIEW_FILTERS.map(([key, label, read]) => (
          <label key={key}>
            {label}
            <select data-filter={key} value={chosen[key] || ""} onChange={(event) => setChosen({ ...chosen, [key]: event.target.value })}>
              <option value="">All</option>
              {[...new Set(runs.map(read).filter((value) => value !== ""))].sort().map((value) => (
                <option key={value} value={value}>{value}</option>
              ))}
            </select>
          </label>
        ))}
      </div>
      {shown.length === 0 ? <p>No reviews match these filters.</p> : (
        <ul>
          {shown.map((run) => (
            <RunRow key={run.run_id || run.feature} run={run} detailPrefix={detailPrefix} selected={selected !== null && run.run_id === selected} />
          ))}
        </ul>
      )}
    </>
  );
}

const FRESHNESS_BANNERS = {
  stale: { icon: "⚠", label: "STALE", text: "The provider's head differs from the reviewed commit; this report may be out of date." },
  unverified: { icon: "?", label: "UNVERIFIED", text: "The provider's head could not be checked against the reviewed commit." },
};

// The server decides; anything it does not clearly call current or stale is unverified.
function freshnessOf(report) {
  return report.freshness === "current" || report.freshness === "stale" ? report.freshness : "unverified";
}

function shortHash(value) {
  return typeof value === "string" && /^[0-9a-f]{4,64}$/i.test(value) ? value : "none";
}

function FreshnessBanner({ report, freshness }) {
  const banner = FRESHNESS_BANNERS[freshness];
  if (banner === undefined) {
    return null;
  }
  return (
    <div className={"banner freshness-banner " + freshness} role="status" data-freshness={freshness}>
      <strong>
        <span aria-hidden="true">{banner.icon} </span>
        {banner.label}
      </strong>
      <p>{banner.text}</p>
      <p>{"Reviewed commit: " + shortHash(report.reviewed_head_sha)}</p>
      <p>{"Provider head: " + shortHash(report.provider_head_sha)}</p>
    </div>
  );
}

function Detail({ report, listPath = "/" }) {
  const freshness = freshnessOf(report);
  const href = freshness === "current" && report.link_allowed === true ? safeHttpsLink(report.link) : null;
  return (
    <>
      <button type="button" className="back-to-list" data-back="list" onClick={() => navigate(listPath)}>
        Back to the list
      </button>
      <Generated at={report.generated_at} />
      <FreshnessBanner report={report} freshness={freshness} />
      {href !== null && (
        <a href={href} rel="noopener noreferrer" referrerPolicy="no-referrer">
          Open at the provider
        </a>
      )}
      {DETAIL_SECTIONS.map(([title, key]) => (
        <section key={key}>
          <h2>{title}</h2>
          <ul>
            {(report[key].length === 0 ? ["none recorded"] : report[key]).map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}

// Reads one page of data. Only a refused token (401) asks for the token again. A server
// that cannot be reached shows a Disconnected banner, any other failed or unreadable reply an
// error banner, both with a retry and both keeping what was already loaded, marked Outdated.
// 404 and 403 give the same generic Not found screen. A slow answer for a page that is no
// longer showing is dropped.
// The one request the page makes: a GET of a fixed same-origin path with the token in a header.
// A passkey session is the cookie alone, so it sends no header; the token path sends the token.
const SESSION = Symbol("passkey session");

function get(path, token, signal) {
  const headers = token === SESSION ? {} : { Authorization: "Bearer " + token };
  return fetch(path, { method: "GET", headers, cache: "no-store", referrerPolicy: "no-referrer", signal });
}

function NotFound() {
  return (
    <div className="card not-found">
      <h2>Not found</h2>
      <p>This page does not exist or is not available to you.</p>
      <button type="button" onClick={() => navigate("/")}>
        Back to the list
      </button>
    </div>
  );
}

const REFRESH_MS = 30000;

function checkedTime() {
  return new Date().toISOString().replace(/\.\d+Z$/, "Z");
}

function Loaded({ token, path, loading, parse, draw, onSignedOut, reloads = 0 }) {
  const [state, setState] = useState({ kind: "loading", data: null });
  const [attempt, setAttempt] = useState(0);
  // True while a request of this page is waiting, so a refresh never starts on top of it.
  const busy = useRef(false);
  // About every 30 seconds, and as soon as the tab is shown again, ask once more; a hidden tab or a
  // request still waiting is left alone. Leaving the page stops it.
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === "visible" && !busy.current) setAttempt((count) => count + 1);
    };
    const timer = setInterval(refresh, REFRESH_MS);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);
  useEffect(() => {
    let current = true;
    busy.current = true;
    // Leaving the page (or asking again) cancels the request, so nothing outlives its screen.
    const controller = new AbortController();
    const fail = (kind) => setState((previous) => ({ kind, data: previous.data, checked: checkedTime() }));
    setState((previous) => (previous.data === null ? { kind: "loading", data: null } : previous));
    (async () => {
      try {
        const response = await get(path, token, controller.signal);
        if (!current) return;
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        if (response.status === 404 || response.status === 403) {
          setState({ kind: "notfound", data: null });
          return;
        }
        if (!response.ok) {
          fail("error");
          return;
        }
        let data = null;
        try {
          data = parse(await response.json());
        } catch (unreadable) {
          data = null;
        }
        if (!current) return;
        if (data === null) fail("error");
        else setState({ kind: "ready", data, checked: checkedTime() });
      } catch (failure) {
        if (current) fail("disconnected");
      } finally {
        if (current) busy.current = false;
      }
    })();
    return () => {
      current = false;
      controller.abort();
    };
  }, [token, path, attempt, reloads]);
  if (state.kind === "notfound") {
    return <NotFound />;
  }
  if (state.kind === "loading") {
    return (
      <div className="skeleton" role="status" aria-busy="true">
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="sr-only">{loading}</span>
      </div>
    );
  }
  if (state.kind === "ready") {
    return (
      <>
        {draw(state.data)}
        <p className="last-checked" data-last-checked>
          Last checked <Moment at={state.checked} />
        </p>
      </>
    );
  }
  return (
    <>
      <div className="banner" role="alert">
        {state.kind === "disconnected" ? <p>Disconnected. Last checked <Moment at={state.checked} /></p> : <p>The inbox could not be read just now.</p>}
        {state.data !== null ? <Badge kind="freshness-outdated" /> : null}
        <button type="button" onClick={() => setAttempt(attempt + 1)}>
          Try again
        </button>
      </div>
      {state.data !== null ? draw(state.data) : null}
    </>
  );
}

function isCount(value) {
  return value === null || (Number.isInteger(value) && value >= 0);
}

function isProject(project) {
  return project !== null && typeof project === "object" && typeof project.id === "string" && RUN_ID.test(project.id) && typeof project.name === "string" && typeof project.available === "boolean" && ["features", "active_runs", "reviews_waiting", "failed_runs"].every((key) => isCount(project[key]));
}

function parseProjects(data) {
  return data && Array.isArray(data.projects) && data.projects.every(isProject) ? data.projects : null;
}

function ProjectCard({ project }) {
  return (
    <li className="project card">
      <button type="button" onClick={() => navigate("/projects/" + project.id)}>
        {project.name}
      </button>
      {project.available ? (
        <div className="counts">
          <p>{"Features: " + project.features}</p>
          <p>{"Active runs: " + project.active_runs}</p>
          <p>{"Reviews waiting: " + project.reviews_waiting}</p>
          <p>{"Failed runs (last " + (Number.isInteger(project.failed_window_hours) ? project.failed_window_hours : 24) + "h): " + project.failed_runs}</p>
        </div>
      ) : (
        <em>Unavailable</em>
      )}
    </li>
  );
}

function Dashboard({ projects }) {
  if (projects.length === 0) {
    return <p>No projects found.</p>;
  }
  return (
    <ul className="projects">
      {projects.map((project) => (
        <ProjectCard key={project.id} project={project} />
      ))}
    </ul>
  );
}

function PageHeader({ title, subtitle, onRefresh }) {
  return (
    <header className="page-header">
      <div>
        <h1>{title}</h1>
        <p className="subtitle">{subtitle}</p>
      </div>
      {onRefresh ? (
        <button type="button" className="icon-button" data-refresh aria-label="Refresh" title="Refresh" onClick={onRefresh}>
          <Icon name="refresh" />
        </button>
      ) : null}
    </header>
  );
}

const SETTING_FIELDS = [["Harness", "harness"], ["Model", "model"], ["Effort", "effort"]];

function isAgent(agent) {
  return agent === null || (typeof agent === "object" && SETTING_FIELDS.every(([, key]) => agent[key] === null || typeof agent[key] === "string"));
}

function parseSettings(data) {
  const settings = data && data.settings;
  const ok = settings && typeof settings === "object" && isAgent(settings.implementer) && isAgent(settings.reviewer) && Array.isArray(settings.models) && settings.models.every((model) => model !== null && typeof model === "object" && typeof model.name === "string" && isAgent(model));
  return ok && settings.implementer !== undefined && settings.reviewer !== undefined ? settings : null;
}

// One agent's fields as labelled text; a value the server did not give is a dash that says so.
function AgentFields({ agent }) {
  return (
    <dl className="settings-fields">
      {SETTING_FIELDS.map(([label, key]) => (
        <div key={key}>
          <dt>{label}</dt>
          <dd>{agent && agent[key] !== null ? agent[key] : "—"}{agent && agent[key] !== null ? null : <span className="muted"> (not set)</span>}</dd>
        </div>
      ))}
    </dl>
  );
}

// Settings, signed in with a passkey: the passkeys on the server and a way to add another one.
function PasskeysCard() {
  const [keys, setKeys] = useState(null);
  const load = () => listPasskeys().then(setKeys);
  useEffect(() => {
    load();
  }, []);
  if (keys === null) return null;
  const failed = keys === "failed";
  return (
    <section className="card passkeys">
      <h2>Passkeys</h2>
      {failed && (
        <>
          <p role="alert">The passkeys could not be listed.</p>
          <button type="button" onClick={load}>
            Try again
          </button>
        </>
      )}
      <ul>
        {(failed ? [] : keys).map((key) => (
          <li key={key.id}>
            <strong>{key.label}</strong> <code>{key.id}</code>
          </li>
        ))}
      </ul>
      {passkeysAvailable() && <EnrolPasskey token={null} another onDone={load} />}
    </section>
  );
}

function SettingsView({ settings }) {
  return (
    <>
      <section className="card"><h2>Remote actions</h2><p>{`Remote actions: ${settings.remote_actions === true ? "On" : "Off"}`}</p></section>
      <section className="card"><h2>Implementer</h2><AgentFields agent={settings.implementer} /></section>
      <section className="card"><h2>Reviewer</h2><AgentFields agent={settings.reviewer} /></section>
      <section className="card">
        <h2>Models</h2>
        {settings.models.length === 0 ? <p>No models configured.</p> : settings.models.map((model) => (
          <div key={model.name}>
            <h3>{model.name}</h3>
            <AgentFields agent={model} />
          </div>
        ))}
      </section>
    </>
  );
}

const FEATURE_GROUPS = ["draft", "pending", "in_progress", "completed", "abandoned"];

function isFeature(feature) {
  return feature !== null && typeof feature === "object" && typeof feature.name === "string" && typeof feature.status === "string" && (feature.description === null || typeof feature.description === "string") && isTextList(feature.depends_on);
}

function parseFeatures(data) {
  return data && Array.isArray(data.features) && data.features.every(isFeature) ? data.features : null;
}

// A feature's dependencies: a link for each one in this ledger, a marker for one that is not.
function Dependencies({ names, known, onOpen, listPath }) {
  return (
    <ul className="dependencies">
      {names.map((name) => (
        <li key={name}>
          {known.has(name) ? (
            <a href={listPath} data-dependency={name} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); onOpen(name); } }}>{name}</a>
          ) : (
            <span className="missing-dependency">{name}<em>{" — Missing dependency"}</em></span>
          )}
        </li>
      ))}
    </ul>
  );
}

// Features grouped by status in the order work moves through, a status the page does not know last.
// Dependencies are folded until asked for; a link to one opens that feature, clearing any filter
// that would hide it.
function FeatureList({ features, listPath }) {
  const [query, setQuery] = useState("");
  const [only, setOnly] = useState("all");
  const [open, setOpen] = useState(() => new Set());
  const [reveal, setReveal] = useState(null);
  useEffect(() => {
    if (reveal === null) {
      return;
    }
    const target = document.getElementById("feature-" + reveal);
    if (target !== null) {
      if (typeof target.scrollIntoView === "function") target.scrollIntoView({ block: "nearest" });
      target.focus();
    }
    setReveal(null);
  }, [reveal]);
  if (features.length === 0) {
    return <p>No features in this project's ledger.</p>;
  }
  const known = new Set(features.map((feature) => feature.name));
  const statuses = [...FEATURE_GROUPS.filter((status) => features.some((feature) => feature.status === status)), ...[...new Set(features.map((feature) => feature.status))].filter((status) => !FEATURE_GROUPS.includes(status))];
  const needle = query.trim().toLowerCase();
  const shown = features.filter((feature) => (only === "all" || feature.status === only) && feature.name.toLowerCase().includes(needle));
  const toggle = (name) => setOpen((was) => { const next = new Set(was); if (next.has(name)) next.delete(name); else next.add(name); return next; });
  const openDependency = (name) => {
    const target = features.find((feature) => feature.name === name);
    if (target !== undefined && !target.name.toLowerCase().includes(needle)) setQuery("");
    if (target !== undefined && only !== "all" && target.status !== only) setOnly("all");
    setOpen((was) => new Set(was).add(name));
    setReveal(name);
  };
  return (
    <>
      <div className="feature-filters">
        <input type="search" data-feature-search aria-label="Search features" placeholder="Search features" autoComplete="off" spellCheck={false} value={query} onChange={(event) => setQuery(event.target.value)} />
        <select data-feature-status aria-label="Status" value={only} onChange={(event) => setOnly(event.target.value)}>
          <option value="all">All statuses</option>
          {statuses.map((status) => (
            <option key={status} value={status}>{status}</option>
          ))}
        </select>
      </div>
      {shown.length === 0 ? <p>No features match the filters.</p> : null}
      {statuses.filter((status) => shown.some((feature) => feature.status === status)).map((status) => (
        <section key={status} className="feature-group">
          <h2 className="feature-status">{status}</h2>
          <ul>
            {shown
              .filter((feature) => feature.status === status)
              .map((feature) => (
                <li key={feature.name} id={"feature-" + feature.name} tabIndex={-1} className="feature card">
                  <strong>{feature.name}</strong>
                  {feature.description ? <p>{feature.description}</p> : null}
                  {feature.depends_on.length > 0 ? (
                    <button type="button" aria-expanded={open.has(feature.name)} onClick={() => toggle(feature.name)}>{"Dependencies (" + feature.depends_on.length + ")"}</button>
                  ) : null}
                  {feature.depends_on.length > 0 && open.has(feature.name) ? <Dependencies names={feature.depends_on} known={known} onOpen={openDependency} listPath={listPath} /> : null}
                </li>
              ))}
          </ul>
        </section>
      ))}
    </>
  );
}

const RUN_STATES = { running: "Running", completed: "Completed", failed: "Failed", blocked: "Blocked", needs_review: "Needs review", pause_requested: "Pause requested", paused: "Paused", merged: "Merged" };

function isOrchestration(run) {
  const text = (value) => value === null || typeof value === "string";
  return run !== null && typeof run === "object" && typeof run.run_id === "string" && typeof run.feature === "string" && typeof run.state === "string" && text(run.stage) && text(run.started_at) && text(run.updated_at) && text(run.target_branch);
}

function parseOrchestrations(data) {
  return data && Array.isArray(data.runs) && data.runs.every(isOrchestration) ? data.runs : null;
}

// Needs-attention first when sorted by state; a state the page does not know goes last.
const STATE_ORDER = ["running", "pause_requested", "paused", "needs_review", "blocked", "failed", "completed", "merged"];

function byState(runs) {
  const rank = (run) => {
    const at = STATE_ORDER.indexOf(run.state);
    return at === -1 ? STATE_ORDER.length : at;
  };
  // Array.prototype.sort is stable, so runs of one state keep the server's newest-first order.
  return [...runs].sort((a, b) => rank(a) - rank(b));
}

// The runs as the server ordered them (newest first), or grouped by state when asked. A state the
// page does not know is "Unknown", never a success.
function runAddress(runId) {
  return currentPath().replace(/\/+$/, "") + "/" + encodeURIComponent(runId);
}

function OrchestrationList({ runs }) {
  const [sort, setSort] = useState("newest");
  if (runs.length === 0) {
    return <p>No runs in this project yet.</p>;
  }
  const running = runs.filter((run) => run.state === "running").length;
  const shown = sort === "state" ? byState(runs) : runs;
  const choices = [["newest", "Newest first"], ["state", "By state"]];
  return (
    <>
      <p>{runs.length + (runs.length === 1 ? " run" : " runs") + " · " + running + " running"}</p>
      <div role="group" aria-label="Sort runs">
        {choices.map(([key, label]) => (
          <button key={key} type="button" data-sort={key} aria-pressed={sort === key} onClick={() => setSort(key)}>{label}</button>
        ))}
      </div>
      <ul>
        {shown.map((run) => (
          <li key={run.run_id} className="run card">
            <strong>{run.feature}</strong>
            {RUN_ID.test(run.run_id) ? <button type="button" data-open={run.run_id} onClick={() => navigate(runAddress(run.run_id))}>Open timeline</button> : null}
            <span className="run-state">{Object.hasOwn(RUN_STATES, run.state) ? RUN_STATES[run.state] : "Unknown"}</span>
            {run.stage ? <p>{"Stage: " + run.stage}</p> : null}
            <p className="run-id">{"Run " + run.run_id + (run.target_branch ? " into " + run.target_branch : "")}</p>
            <Stamp label="Last activity" at={run.updated_at} />
            <Stamp label="Started" at={run.started_at} />
          </li>
        ))}
      </ul>
    </>
  );
}

function isEvent(event) {
  const text = (value) => value === null || typeof value === "string";
  return event !== null && typeof event === "object" && typeof event.event_id === "string" && typeof event.kind === "string" && text(event.stage) && Number.isInteger(event.attempt) && text(event.occurred_at);
}

function parseTimeline(data) {
  const ok = data && isOrchestration(data.run) && Array.isArray(data.events) && data.events.every(isEvent) && (data.next_cursor === null || (typeof data.next_cursor === "string" && RUN_ID.test(data.next_cursor)));
  return ok ? data : null;
}

// One run: its header, then a page of its events in the order they happened. The next page is
// its own address, so every request stays one plain GET.
function RunTimeline({ timeline, listPath, runPath, follow }) {
  const { run, events, next_cursor: next } = timeline;
  return (
    <>
      <button type="button" data-back="list" onClick={() => navigate(listPath)}>Back to the runs</button>
      <h2>{run.feature}</h2>
      <span className="run-state">{Object.hasOwn(RUN_STATES, run.state) ? RUN_STATES[run.state] : "Unknown"}</span>
      {run.stage ? <p>{"Stage: " + run.stage}</p> : null}
      <p className="run-id">{"Run " + run.run_id + (run.target_branch ? " into " + run.target_branch : "")}</p>
      <Stamp label="Started" at={run.started_at} />
      <Stamp label="Last activity" at={run.updated_at} />
      <button type="button" data-open="logs" onClick={() => navigate(runPath + "/logs")}>View logs</button>
      {events.length === 0 ? <p>No events recorded.</p> : null}
      <ol>
        {events.map((event) => (
          <li key={event.event_id} className="event card">
            <strong>{event.kind}</strong>
            {event.stage ? <span>{"Stage: " + event.stage}</span> : null}
            <span>{"Attempt " + event.attempt}</span>
            <Moment at={event.occurred_at} />
          </li>
        ))}
      </ol>
      {next !== null && <button type="button" data-more="events" onClick={() => navigate(runPath + "/after/" + encodeURIComponent(next))}>Load more events</button>}
      {next === null && follow !== null ? <FollowLive key={follow.base} token={follow.token} base={follow.base} from={events.length > 0 ? events[events.length - 1].event_id : follow.cursor} shown={events} onSignedOut={follow.onSignedOut} /> : null}
    </>
  );
}

const RECONNECT_MS = 3000;
// A block of the event stream that carries an event: its `data:` line, read as an event.
function streamedEvent(block) {
  const line = block.split("\n").find((text) => text.startsWith("data: "));
  if (line === undefined) return null;
  try {
    const event = JSON.parse(line.slice(6));
    return isEvent(event) && RUN_ID.test(event.event_id) ? event : null;
  } catch (unreadable) {
    return null;
  }
}

// The events that came after the page was drawn, read from the run's event stream with the token in
// the header (the browser's own stream object cannot send one). A stream that ends or breaks is
// opened again from the last event seen, and an event already shown is never shown twice.
function FollowLive({ token, base, from, shown, onSignedOut }) {
  const [more, setMore] = useState([]);
  const [status, setStatus] = useState("live");
  const [retry, setRetry] = useState(0);
  const last = useRef(from);
  // Every event id accepted so far, so a repeat is told apart in constant time.
  const ids = useRef(new Set());
  useEffect(() => {
    let current = true;
    let timer = null;
    const controller = new AbortController();
    const path = base + "/events" + (last.current === null ? "" : "/after/" + encodeURIComponent(last.current));
    const again = () => {
      if (!current) return;
      setStatus("reconnecting");
      timer = setTimeout(() => setRetry((count) => count + 1), RECONNECT_MS);
    };
    (async () => {
      try {
        const response = await get(path, token, controller.signal);
        if (!current) return;
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        if (response.status === 404 || response.status === 403) {
          setStatus("stopped");
          return;
        }
        if (!response.ok || !response.body) {
          again();
          return;
        }
        setStatus("live");
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        // A CR ends a line at once; an LF straight after it, even in the next read, is its other half.
        let afterCr = false;
        for (;;) {
          const { done, value } = await reader.read();
          if (!current) return;
          if (done || buffer.length > 1048576) break;
          let text = decoder.decode(value, { stream: true });
          if (text.length > 0) {
            if (afterCr && text.startsWith("\n")) text = text.slice(1);
            afterCr = text.endsWith("\r");
          }
          buffer += text.replace(/\r\n?/g, "\n");
          const blocks = buffer.split("\n\n");
          buffer = blocks.pop();
          const fresh = [];
          for (const event of blocks.map(streamedEvent)) {
            if (event === null || ids.current.has(event.event_id)) continue;
            ids.current.add(event.event_id);
            fresh.push(event);
          }
          if (fresh.length > 0) {
            last.current = fresh[fresh.length - 1].event_id;
            setMore((known) => [...known, ...fresh]);
          }
        }
        again();
      } catch (failure) {
        again();
      }
    })();
    return () => {
      current = false;
      controller.abort();
      if (timer !== null) clearTimeout(timer);
    };
  }, [token, base, retry]);
  const have = new Set(shown.map((event) => event.event_id));
  return (
    <>
      <ol>
        {more.filter((event) => !have.has(event.event_id)).map((event) => (
          <li key={event.event_id} className="event card">
            <strong>{event.kind}</strong>
            {event.stage ? <span>{"Stage: " + event.stage}</span> : null}
            <span>{"Attempt " + event.attempt}</span>
            <Moment at={event.occurred_at} />
          </li>
        ))}
      </ol>
      <p className="live-status" data-live>{status === "live" ? "Following live" : status === "stopped" ? "Live updates stopped" : "Reconnecting…"}</p>
    </>
  );
}

// A log file the server could have listed: a `.log` name that is not hidden and holds no path
// separator, exactly the server's own rule. The name is always encoded into the address.
const LOG_FILE = /^[^./\\][^/\\]*\.log$/;
const LINE_CURSOR = /^[0-9]{1,12}$/;

function parseLogFiles(data) {
  const ok = data && Array.isArray(data.files) && data.files.every((file) => file !== null && typeof file === "object" && typeof file.name === "string" && Number.isInteger(file.bytes));
  return ok ? data.files : null;
}

function parseLogPage(data) {
  const ok = data && typeof data.name === "string" && Array.isArray(data.lines) && data.lines.every((line) => typeof line === "string") && (data.next_cursor === null || (typeof data.next_cursor === "string" && LINE_CURSOR.test(data.next_cursor))) && typeof data.truncated === "boolean";
  return ok ? data : null;
}

function LogFiles({ files, runPath }) {
  return (
    <>
      <button type="button" data-back="run" onClick={() => navigate(runPath)}>Back to the run</button>
      {files.length === 0 ? <p>No logs for this run.</p> : null}
      <ul>
        {files.map((file) => (
          <li key={file.name} className="log-file card">
            <strong>{file.name}</strong>
            <span>{file.bytes + " bytes"}</span>
            {LOG_FILE.test(file.name) ? <button type="button" data-file={file.name} onClick={() => navigate(runPath + "/logs/" + encodeURIComponent(file.name))}>Show</button> : null}
          </li>
        ))}
      </ul>
    </>
  );
}

// A line's level is the first level word in it, whatever its case; "warning" counts as "warn".
const LOG_LEVELS = ["error", "warn", "info", "debug"];

function lineLevel(line) {
  const found = /\b(error|warn|warning|info|debug)\b/i.exec(line);
  if (found === null) return "other";
  const name = found[1].toLowerCase();
  return name === "warning" ? "warn" : name;
}

// The lines holding the text (any case) and, unless the level is "all", of that level. Done in the page only.
function filterLines(lines, text, level) {
  const wanted = text.toLowerCase();
  return lines.filter((line) => (wanted === "" || line.toLowerCase().includes(wanted)) && (level === "all" || lineLevel(line) === level));
}

// Hands the browser the shown lines as a plain-text file, then releases its temporary address.
function saveLines(name, lines) {
  const url = URL.createObjectURL(new Blob([lines.map((line) => line + "\n").join("")], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

// Holds a log file's filters above the page that is loaded, so they stay while its next page opens.
// It is keyed by the file, so another file starts unfiltered.
function LogView({ token, onSignedOut, reloads, pagePath, logsPath, filePath }) {
  const [text, setText] = useState("");
  const [level, setLevel] = useState("all");
  const filters = { text, setText, level, setLevel };
  return <Loaded key={pagePath} reloads={reloads} token={token} path={pagePath} loading="Loading the log…" parse={parseLogPage} draw={(page) => <LogPage page={page} logsPath={logsPath} filePath={filePath} filters={filters} />} onSignedOut={onSignedOut} />;
}

// One page of a log's already-redacted lines, drawn as plain text. The next page is its own address.
function LogPage({ page, logsPath, filePath, filters: { text, setText, level, setLevel } }) {
  const { lines, next_cursor: next, truncated } = page;
  const shown = filterLines(lines, text, level);
  return (
    <>
      <button type="button" data-back="logs" onClick={() => navigate(logsPath)}>Back to the logs</button>
      <h2>{page.name}</h2>
      <div className="log-tools">
        <input type="search" data-log-filter aria-label="Filter lines" placeholder="Filter lines" autoComplete="off" spellCheck={false} value={text} onChange={(event) => setText(event.target.value)} />
        <select data-log-level aria-label="Level" value={level} onChange={(event) => setLevel(event.target.value)}>
          <option value="all">All levels</option>
          {LOG_LEVELS.map((name) => (
            <option key={name} value={name}>{name}</option>
          ))}
        </select>
        <button type="button" data-log-download disabled={shown.length === 0} onClick={() => saveLines(LOG_FILE.test(page.name) ? page.name : "log.log", shown)}>Download</button>
        <span>{shown.length + " of " + lines.length + " lines"}</span>
      </div>
      {lines.length === 0 ? <p>This log has no more lines.</p> : null}
      {lines.length > 0 && shown.length === 0 ? <p>No lines match the filter.</p> : null}
      <pre className="log-lines">
        {shown.map((line, at) => (
          <div key={at}>{line}</div>
        ))}
      </pre>
      {next !== null && <button type="button" data-more="lines" onClick={() => navigate(filePath + "/after/" + encodeURIComponent(next))}>Load more lines</button>}
      {truncated && next === null ? <p>The log was cut short: only its first part is shown.</p> : null}
    </>
  );
}

// What a project's address shows. Only the Reviews tab is built: its list and reports come from
// the project's own endpoints and stay under the project's address.
function ProjectPage({ token, onSignedOut, projectId, path: address, reloads, onRefresh }) {
  if (/^\/projects\/[^/]+\/features$/.test(address)) {
    const featuresPath = "/api/projects/" + encodeURIComponent(projectId) + "/features";
    return (
      <>
        <PageHeader title="Features" subtitle="The project's feature ledger" onRefresh={onRefresh} />
        <Loaded key={featuresPath} reloads={reloads} token={token} path={featuresPath} loading="Loading the features…" parse={parseFeatures} draw={(features) => <FeatureList features={features} listPath={"/projects/" + encodeURIComponent(projectId) + "/features"} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  if (/^\/projects\/[^/]+\/orchestrations$/.test(address)) {
    const runsPath = "/api/projects/" + encodeURIComponent(projectId) + "/runs";
    return (
      <>
        <PageHeader title="Orchestrations" subtitle="The project's orchestration runs" onRefresh={onRefresh} />
        <Loaded key={runsPath} reloads={reloads} token={token} path={runsPath} loading="Loading the runs…" parse={parseOrchestrations} draw={(runs) => <OrchestrationList runs={runs} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  const logs = /^\/projects\/[^/]+\/orchestrations\/([^/]+)\/logs(?:\/([^/]+)(?:\/after\/([^/]+))?)?$/.exec(address);
  if (logs !== null) {
    let run = null;
    let file = null;
    let cursor = null;
    try {
      run = decodeURIComponent(logs[1]);
      file = logs[2] === undefined ? null : decodeURIComponent(logs[2]);
      cursor = logs[3] === undefined ? null : decodeURIComponent(logs[3]);
    } catch (malformed) {
      return <NotFound />;
    }
    if (!RUN_ID.test(run) || (file !== null && !LOG_FILE.test(file)) || (cursor !== null && !LINE_CURSOR.test(cursor))) return <NotFound />;
    const runPath = "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(run);
    const logsPath = runPath + "/logs";
    const apiRun = "/api/projects/" + encodeURIComponent(projectId) + "/runs/" + encodeURIComponent(run) + "/logs";
    if (file === null) {
      return (
        <>
          <PageHeader title="Logs" subtitle="The run's log files" onRefresh={onRefresh} />
          <Loaded key={apiRun} reloads={reloads} token={token} path={apiRun} loading="Loading the logs…" parse={parseLogFiles} draw={(files) => <LogFiles files={files} runPath={runPath} />} onSignedOut={onSignedOut} />
        </>
      );
    }
    const filePath = logsPath + "/" + encodeURIComponent(file);
    const pagePath = apiRun + "/" + encodeURIComponent(file) + (cursor === null ? "" : "/after/" + encodeURIComponent(cursor));
    return (
      <>
        <PageHeader title="Log" subtitle={file} onRefresh={onRefresh} />
        <LogView key={filePath} token={token} onSignedOut={onSignedOut} reloads={reloads} pagePath={pagePath} logsPath={logsPath} filePath={filePath} />
      </>
    );
  }
  const timeline = /^\/projects\/[^/]+\/orchestrations\/([^/]+)(?:\/after\/([^/]+))?$/.exec(address);
  if (timeline !== null) {
    let run = null;
    let cursor = null;
    try {
      run = decodeURIComponent(timeline[1]);
      cursor = timeline[2] === undefined ? null : decodeURIComponent(timeline[2]);
    } catch (malformed) {
      return <NotFound />;
    }
    if (!RUN_ID.test(run) || (cursor !== null && !RUN_ID.test(cursor))) return <NotFound />;
    const listPath = "/projects/" + encodeURIComponent(projectId) + "/orchestrations";
    const runPath = listPath + "/" + encodeURIComponent(run);
    const timelinePath = "/api/projects/" + encodeURIComponent(projectId) + "/runs/" + encodeURIComponent(run) + (cursor === null ? "" : "/after/" + encodeURIComponent(cursor));
    return (
      <>
        <PageHeader title="Run" subtitle="The run's timeline" onRefresh={onRefresh} />
        <Loaded key={timelinePath} reloads={reloads} token={token} path={timelinePath} loading="Loading the run…" parse={parseTimeline} draw={(data) => <RunTimeline timeline={data} listPath={listPath} runPath={runPath} follow={{ token, onSignedOut, cursor, base: "/api/projects/" + encodeURIComponent(projectId) + "/runs/" + encodeURIComponent(run) }} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  const match = /^\/projects\/[^/]+\/reviews(?:\/([^/]*))?$/.exec(address);
  if (match === null) return <p>This page is not available yet.</p>;
  const base = "/api/projects/" + encodeURIComponent(projectId) + "/reviews";
  const listPath = "/projects/" + encodeURIComponent(projectId) + "/reviews";
  if (match[1] !== undefined && !RUN_ID.test(match[1])) return <NotFound />;
  const open = match[1];
  const path = open === undefined ? null : base + "/" + encodeURIComponent(open);
  // One tree for both addresses: the list stays mounted beside the report (the stylesheet hides it
  // on a phone), so choosing another report never fetches the list again.
  return (
    <>
      {open === undefined && <PageHeader title="Reviews" subtitle="Code reviews and agent reports" onRefresh={onRefresh} />}
      <div className={open === undefined ? "master-detail" : "master-detail has-report"}>
        <div className="master-pane">
          <Loaded key={base} reloads={reloads} token={token} path={base} loading="Loading the review inbox…" parse={parseRuns} draw={(runs) => <RunList runs={runs} detailPrefix={listPath + "/"} selected={open === undefined ? null : open} />} onSignedOut={onSignedOut} />
        </div>
        {path !== null && (
          <div className="detail-pane">
            <Loaded key={path} token={token} path={path} loading="Loading the report…" parse={parseDetail} draw={(report) => <Detail report={report} listPath={listPath} />} onSignedOut={onSignedOut} />
          </div>
        )}
      </div>
    </>
  );
}

function SignedIn({ token, onSignedOut, projectId = null, projects = null }) {
  const [reloads, setReloads] = useState(0);
  const hash = useSyncExternalStore(subscribeToLocation, currentPath);
  if (projectId !== null && projects !== null && !projects.some((project) => project.id === projectId)) {
    return <NotFound />;
  }
  if (projectId !== null) {
    return <ProjectPage token={token} onSignedOut={onSignedOut} projectId={projectId} path={hash} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} />;
  }
  if (UNBUILT_DESTINATIONS.some((prefix) => hash === prefix || hash.startsWith(prefix + "/"))) {
    return <p>This page is not available yet.</p>;
  }
  if (hash.startsWith("/settings/")) {
    return <NotFound />;
  }
  if (hash === "/settings") {
    return (
      <>
        <PageHeader title="Settings" subtitle="Run settings, read only" onRefresh={() => setReloads(reloads + 1)} />
        <PasskeysCard />
        <Loaded key={API_SETTINGS} reloads={reloads} token={token} path={API_SETTINGS} loading="Loading the settings…" parse={parseSettings} draw={(settings) => <SettingsView settings={settings} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  if (hash === "/dashboard") {
    return (
      <>
        <PageHeader title="Dashboard" subtitle="Your projects" onRefresh={() => setReloads(reloads + 1)} />
        <Loaded key={API_PROJECTS} reloads={reloads} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => <Dashboard projects={projects} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  const runId = hash.startsWith(DETAIL_PREFIX) ? hash.slice(DETAIL_PREFIX.length) : "";
  if (RUN_ID.test(runId)) {
    const path = API_REVIEWS + "/" + encodeURIComponent(runId);
    return <Loaded key={path} token={token} path={path} loading="Loading the report…" parse={parseDetail} draw={(report) => <Detail report={report} />} onSignedOut={onSignedOut} />;
  }
  return (
    <>
      <PageHeader title="Reviews" subtitle="Code reviews and agent reports" onRefresh={() => setReloads(reloads + 1)} />
      <Loaded key={API_REVIEWS} reloads={reloads} token={token} path={API_REVIEWS} loading="Loading the review inbox…" parse={parseRuns} draw={(runs) => <RunList runs={runs} />} onSignedOut={onSignedOut} />
    </>
  );
}

// Which project the address names, and the list of projects the sidebar switches between. The list
// is read only inside a project, and a list that cannot be read just hides the switcher.
function Scoped({ token, onSignedOut, onSignOut }) {
  const path = useSyncExternalStore(subscribeToLocation, currentPath);
  const [projects, setProjects] = useState(null);
  const match = /^\/projects\/([^/]+)/.exec(path);
  let named = null;
  try {
    named = match ? decodeURIComponent(match[1]) : null;
  } catch (malformed) {
    named = null;
  }
  const inProject = named !== null;
  useEffect(() => {
    if (!inProject) {
      // Leaving the project forgets its list, so the next visit is never judged by an old one.
      setProjects(null);
      return undefined;
    }
    let current = true;
    const controller = new AbortController();
    (async () => {
      try {
        const response = await get(API_PROJECTS, token, controller.signal);
        if (!current) return;
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        const list = response.ok ? parseProjects(await response.json()) : null;
        if (current) setProjects(list);
      } catch (failure) {
        if (current) setProjects(null);
      }
    })();
    return () => {
      current = false;
      controller.abort();
    };
  }, [token, inProject]);
  const known = projects !== null && projects.some((project) => project.id === named);
  return (
    <Shell projectId={projects === null || known ? named : null} projects={projects} onSignOut={onSignOut}>
      <SignedIn token={token} onSignedOut={onSignedOut} projectId={named} projects={projects} />
    </Shell>
  );
}

// Storage can be missing or refuse to work (private windows, blocked site data); the page then
// simply keeps the token for this page view alone.
const TOKEN_KEY = "orq.token";

function keptToken() {
  try {
    const kept = window.sessionStorage.getItem(TOKEN_KEY);
    return typeof kept === "string" && kept !== "" ? kept : null;
  } catch (unavailable) {
    return null;
  }
}

function keepToken(token) {
  try {
    if (token === null) window.sessionStorage.removeItem(TOKEN_KEY);
    else window.sessionStorage.setItem(TOKEN_KEY, token);
  } catch (unavailable) {
    // nothing to keep it in
  }
}

// Offered once the page is signed in with the token and no passkey exists yet: the token then
// enrols the first one, and later visits sign in with it.
function EnrolPasskey({ token, onDone, another = false }) {
  const [state, setState] = useState("idle");
  const go = async () => {
    setState("working");
    let ok = false;
    try {
      ok = await registerPasskey(typeof token === "string" ? token : null, "Passkey " + new Date().toISOString().slice(0, 16).replace("T", " "));
    } catch (cancelled) {
      ok = false;
    }
    setState(ok ? "idle" : "failed");
    if (ok) onDone();
  };
  return (
    <div className="card passkey-enrol">
      <button type="button" onClick={go} disabled={state === "working"}>
        {another ? "Add another passkey" : "Add a passkey for this device"}
      </button>
      {state === "failed" && <p role="alert">The passkey could not be added.</p>}
    </div>
  );
}

function App() {
  const [token, setTokenState] = useState(keptToken);
  const [status, setStatus] = useState(null);
  const setToken = (next) => {
    keepToken(next);
    setTokenState(next);
  };
  const [message, setMessage] = useState("");
  const refresh = () => authStatus().then(setStatus);
  useEffect(() => {
    refresh();
  }, []);
  const session = status !== null && status.signed_in === true;
  const active = session && token === null ? SESSION : token;
  if (active === null) {
    return <main><TokenPrompt message={message} onToken={setToken} passkeys={status !== null && status.passkeys} known={status !== null} onPasskey={refresh} blocker={status !== null ? passkeyBlocker() : null} /></main>;
  }
  const signedOut = () => {
    setToken(null);
    setMessage(SIGN_IN_FAILED);
    refresh();
  };
  const signOut = () => {
    setMessage("");
    setToken(null);
    if (session) signOutOfSession().then(refresh);
  };
  const first = typeof active === "string" && status !== null && status.passkeys === false && passkeysAvailable();
  const blocker = status !== null ? passkeyBlocker() : null;
  return (
    <>
      {first && <EnrolPasskey token={active} onDone={refresh} />}
      {blocker !== null && <p className="passkey-blocker" role="status">{blocker}</p>}
      <Scoped token={active} onSignedOut={signedOut} onSignOut={signOut} />
    </>
  );
}

migrateLegacyHash();
createRoot(document.getElementById("app")).render(<App />);
