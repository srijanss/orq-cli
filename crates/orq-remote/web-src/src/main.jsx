// The review inbox page. The bearer token is typed in, held in one piece of state and sent as a
// header on each request. A copy is kept in this tab's sessionStorage only, so a refresh does not
// ask again and closing the tab forgets it; it goes nowhere else (no address, no other storage).
// Every string from the API is rendered as text.
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { Shell } from "./shell.jsx";
import { Badge, BADGES } from "./badge.jsx";
import { Icon } from "./icons.jsx";
import { ThemeChoice, ThemeToggle } from "./theme.jsx";
import { authStatus, confirmWithPasskey, listPasskeys, passkeyBlocker, passkeysAvailable, registerPasskey, signInWithPasskey, signOutOfSession } from "./passkey.js";
import { currentPath, isPlainLeftClick, migrateLegacyHash, navigate, openRootAtDashboard, subscribeToLocation } from "./router.js";

const API_REVIEWS = "/api/reviews";
// One message for every failed sign-in, so it never says which part failed.
const SIGN_IN_FAILED = "Could not sign in. Check the token and try again.";
const STALE_MARKER = "stale, may be out of date";
const UNVERIFIED_MARKER = "unverified, could not be checked";
const DETAIL_PREFIX = "/reviews/";
// Places the sidebar offers that have no page yet; they say so instead of showing the inbox.
const UNBUILT_DESTINATIONS = ["/projects"];
const API_SETTINGS = "/api/settings";
const API_AUDIT = "/api/audit";
const API_PROJECTS = "/api/projects";
// The same plain identifiers the server accepts, checked before one is put in a
// path or an address.
const RUN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const DETAIL_SECTIONS = ["summary", "risks", "files", "tests", "rounds"];

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
  const ok = data !== null && typeof data === "object" && typeof data.stale === "boolean" && DETAIL_SECTIONS.every((key) => isTextList(data[key]));
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
      <button type="button" className="primary" onClick={go}>
        <Icon name="passkey" size={20} />
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
    <div className="signin-page">
    <div className="signin card">
      <span className="brand">
        <Icon name="logo" size={28} />
        Orq
      </span>
      <div className="signin-divider" role="separator" />
      {usable && !asking && <PasskeyButton onSignedIn={onPasskey} enrolled={passkeys} onAskToken={() => setAsking(true)} />}
      {blocker !== null && <p role="status">{blocker}</p>}
      {(!usable || asking) && (
        <>
          <p>Sign in with token: paste the access token printed by orq serve.</p>
          <label>
            Access token
            <input ref={field} type="password" autoComplete="off" spellCheck={false} aria-label="Access token" onKeyDown={(event) => event.key === "Enter" && submit()} />
          </label>
          <button type="button" className="primary" onClick={submit}>
            Open the inbox
              </button>
        </>
      )}
      <p role="alert">{message}</p>
      <div className="alternatives" />
      <ThemeToggle />
    </div>
    </div>
  );
}

// A risk the reference knows gets its coloured badge. A placeholder such as "-" means the review gave
// no rating, which a muted pill says plainly; any other word is shown as it came, in the same shape.
function RiskBadge({ risk }) {
  if (risk === undefined || risk === null) return null;
  const word = String(risk).trim();
  const kind = "risk-" + word.toLowerCase();
  if (Object.hasOwn(BADGES, kind)) return <Badge kind={kind} />;
  const unrated = word === "" || /^(-+|none|n\/a|unknown)$/i.test(word);
  return <span className="badge badge-risk-other">{unrated ? "No risk rating" : word + " risk"}</span>;
}

// The short pill reads "Stale" or "Unverified"; a screen reader hears the full marker instead.
function FreshnessPill({ freshness }) {
  if (freshness !== "stale" && freshness !== "unverified") return null;
  return (
    <span className={"pill pill-" + freshness}>
      <span aria-hidden="true">{freshness === "stale" ? "Stale" : "Unverified"}</span>
      <span className="sr-only">{freshness === "stale" ? STALE_MARKER : UNVERIFIED_MARKER}</span>
    </span>
  );
}

function ReviewRow({ run, detailPrefix, selected = false, showProject = false }) {
  const open = typeof run.run_id === "string" && RUN_ID.test(run.run_id);
  const freshness = rowFreshness(run);
  const body = (
    <>
      <span className="review-row-title">{String(run.feature)}</span>
      <span className="review-row-meta">
        {open ? <code>{run.run_id}</code> : null}
        {showProject && typeof run.project === "string" ? <span>{run.project}</span> : null}
        <Generated at={run.generated_at} />
      </span>
      <span className="review-tags">
        <span className="review-status">{String(run.status)}</span>
        <RiskBadge risk={run.risk} />
        <FreshnessPill freshness={freshness} />
      </span>
    </>
  );
  return (
    <li className={freshness === "stale" ? "review-row stale" : "review-row"} data-selected={selected ? run.run_id : undefined}>
      {open ? (
        <button type="button" className="review-row-open" data-open-run={run.run_id} aria-current={selected ? "true" : undefined} onClick={() => navigate(detailPrefix + run.run_id)}>
          {body}
        </button>
      ) : (
        <div className="review-row-open">{body}</div>
      )}
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

function reviewedToday(run, now) {
  const when = validTime(run.generated_at);
  return when !== null && when.getFullYear() === now.getFullYear() && when.getMonth() === now.getMonth() && when.getDate() === now.getDate();
}

const REVIEW_FILTERS = [
  ["status", "Status", (run) => String(run.status)],
  ["risk", "Risk", (run) => (run.risk === undefined || run.risk === null ? "" : String(run.risk))],
  ["freshness", "Freshness", rowFreshness],
];

// Today's reviews or all of them, newest (or oldest) first; the filters only hide rows, so a
// marker never leaves a row that is shown.
function ReviewList({ runs, detailPrefix = DETAIL_PREFIX, selected = null, onRows = null, showProject = false }) {
  const [chosen, setChosen] = useState({});
  const [range, setRange] = useState("all");
  const [order, setOrder] = useState("desc");
  useEffect(() => {
    if (onRows !== null) onRows(runs);
  }, [runs]);
  if (runs.length === 0) {
    return <p>No reviews in the inbox.</p>;
  }
  const now = new Date();
  const today = runs.filter((run) => reviewedToday(run, now));
  const pool = range === "today" ? today : runs;
  const shown = pool
    .filter((run) => REVIEW_FILTERS.every(([key, , read]) => !chosen[key] || read(run) === chosen[key]))
    .sort((a, b) => (order === "desc" ? reviewTime(b) - reviewTime(a) : reviewTime(a) - reviewTime(b)));
  const flip = order === "desc" ? "asc" : "desc";
  return (
    <div className="review-list card">
      <div className="review-list-head">
        <div className="review-range" role="group" aria-label="Which reviews">
          {[["today", "Today (" + today.length + ")"], ["all", "All"]].map(([id, label]) => (
            <button key={id} type="button" aria-pressed={range === id ? "true" : "false"} data-review-range={id} onClick={() => setRange(id)}>
              {label}
            </button>
          ))}
        </div>
        <button type="button" className="sort-toggle" data-review-sort={order} aria-label={order === "desc" ? "Sort by review time: newest first" : "Sort by review time: oldest first"} title={flip === "asc" ? "Show the oldest first" : "Show the newest first"} onClick={() => setOrder(flip)}>
          <Icon name={order === "desc" ? "sortDesc" : "sortAsc"} size={18} />
          {order === "desc" ? "Sort: Newest" : "Sort: Oldest"}
        </button>
      </div>
      <div className="filters review-filters" role="group" aria-label="Filter reviews">
        {REVIEW_FILTERS.map(([key, label, read]) => (
          <label key={key}>
            <span className="sr-only">{label}</span>
            <select data-filter={key} value={chosen[key] || ""} onChange={(event) => setChosen({ ...chosen, [key]: event.target.value })}>
              <option value="">{label + ": All"}</option>
              {[...new Set(runs.map(read).filter((value) => value !== ""))].sort().map((value) => (
                <option key={value} value={value}>{value}</option>
              ))}
            </select>
          </label>
        ))}
      </div>
      {shown.length === 0 ? (
        <p className="review-empty">{range === "today" && today.length === 0 ? "No reviews today." : "No reviews match these filters."}</p>
      ) : (
        <ul className="review-rows">
          {shown.map((run) => (
            <ReviewRow key={run.run_id || run.feature} run={run} detailPrefix={detailPrefix} selected={selected !== null && run.run_id === selected} showProject={showProject} />
          ))}
        </ul>
      )}
    </div>
  );
}

// The server decides; anything it does not clearly call current or stale is unverified.
function freshnessOf(report) {
  return report.freshness === "current" || report.freshness === "stale" ? report.freshness : "unverified";
}

function shortHash(value) {
  return typeof value === "string" && /^[0-9a-f]{4,64}$/i.test(value) ? value : "none";
}

// Stale names both commits; unverified names only the reviewed one, since the head is not known.
function FreshnessBanner({ report, freshness }) {
  if (freshness !== "stale" && freshness !== "unverified") {
    return null;
  }
  const reviewed = shortHash(report.reviewed_head_sha);
  const text = freshness === "stale"
    ? "The provider head has moved since this review (head: " + shortHash(report.provider_head_sha) + ", reviewed: " + reviewed + ")."
    : "The provider head could not be checked against the reviewed commit (reviewed: " + reviewed + ").";
  return (
    <div className={"freshness-banner " + freshness} role="status" data-freshness={freshness}>
      <Icon name="alert" size={20} />
      <div>
        <strong>{freshness === "stale" ? "STALE: this review may be out of date" : "UNVERIFIED: this review could not be checked"}</strong>
        <p>{text}</p>
      </div>
    </div>
  );
}

const REPORT_SECTIONS = [
  ["Risks", "risks"],
  ["Files changed", "files"],
  ["Tests", "tests"],
  ["Review rounds", "rounds"],
];

function providerName(href) {
  const host = new URL(href).hostname;
  return host === "github.com" ? "GitHub" : host === "gitlab.com" ? "GitLab" : host;
}

// The report beside (or, on a phone, instead of) the list. The row it was opened from, when the
// list has it, gives the feature, status and risk the report itself does not carry.
function Detail({ report, row = null, listPath = "/reviews" }) {
  const freshness = freshnessOf(report);
  const href = freshness === "current" && report.link_allowed === true ? safeHttpsLink(report.link) : null;
  const commit = shortHash(report.reviewed_head_sha);
  return (
    <article className="review-report card">
      <div className="review-report-head">
        <button type="button" className="back-arrow back-to-list" data-back="list" onClick={() => navigate(listPath)}>
          <Icon name="arrowLeft" size={22} />
          <span className="sr-only">Back to the list</span>
        </button>
        <h2>{row !== null ? String(row.feature) : "Review report"}</h2>
        <span className="review-tags">
          <FreshnessPill freshness={freshness} />
          {row !== null ? <RiskBadge risk={row.risk} /> : null}
          {row !== null ? <span className="review-status">{String(row.status)}</span> : null}
        </span>
      </div>
      <p className="review-report-sub">
        {commit === "none" ? null : <code title={commit}>{commit.slice(0, 7)}</code>}
        <Generated at={report.generated_at} />
      </p>
      <FreshnessBanner report={report} freshness={freshness} />
      <section className="review-summary">
        <h3>Summary</h3>
        {(report.summary.length === 0 ? ["none recorded"] : report.summary).map((line, index) => (
          <p key={index}>{line}</p>
        ))}
      </section>
      {REPORT_SECTIONS.map(([title, key]) => (
        <details key={key} className="review-section" open={key === "risks" ? true : undefined}>
          <summary>
            <Icon name="chevronRight" size={16} />
            <span>{title}</span>
            <span className="review-count">{"(" + report[key].length + ")"}</span>
          </summary>
          <ul>
            {(report[key].length === 0 ? ["none recorded"] : report[key]).map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        </details>
      ))}
      {href !== null && (
        <p className="review-provider">
          <span className="muted">Provider:</span>
          <a href={href} rel="noopener noreferrer" referrerPolicy="no-referrer">
            {providerName(href)}
            <Icon name="external" size={16} />
          </a>
        </p>
      )}
    </article>
  );
}

// One page for the global inbox and a project's: the list stays mounted beside the report (the
// stylesheet hides it on a phone), so choosing another report never fetches the list again.
// The project list for the reviews picker on the all-projects page. It is read only when the picker
// is first opened, so the inbox loads alone; a list that cannot be read leaves just "All projects".
function useLateProjects(token, wanted) {
  const [projects, setProjects] = useState(null);
  useEffect(() => {
    if (!wanted) return undefined;
    let current = true;
    const controller = new AbortController();
    (async () => {
      try {
        const response = await get(API_PROJECTS, token, controller.signal);
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
  }, [token, wanted]);
  return projects;
}

function ReviewsPage({ token, onSignedOut, reloads, onRefresh, api, listPath, open, showProject = false, projectId = null, projects = undefined }) {
  const [rows, setRows] = useState(null);
  const [wanted, setWanted] = useState(false);
  const late = useLateProjects(token, projects === undefined && wanted);
  const path = open === undefined ? null : api + "/" + encodeURIComponent(open);
  const row = rows === null || open === undefined ? null : rows.find((candidate) => candidate.run_id === open) ?? null;
  return (
    <>
      <PageHeader title="Reviews" subtitle="Code reviews and agent reports" onRefresh={onRefresh} />
      {projects === undefined
        ? <ProjectSelect projects={late ?? []} projectId={null} section="reviews" onOpen={() => setWanted(true)} />
        : <ProjectSelect projects={projects} projectId={projectId} section="reviews" />}
      <div className={open === undefined ? "master-detail" : "master-detail has-report"}>
        <div className="master-pane">
          <Loaded key={api} reloads={reloads} token={token} path={api} loading="Loading the review inbox…" parse={parseRuns} draw={(runs) => <ReviewList runs={runs} detailPrefix={listPath + "/"} selected={open === undefined ? null : open} onRows={setRows} showProject={showProject} />} onSignedOut={onSignedOut} />
        </div>
        {path !== null && (
          <div className="detail-pane">
            <Loaded key={path} back={listPath} token={token} path={path} loading="Loading the report…" parse={parseDetail} draw={(report) => <Detail report={report} row={row} listPath={listPath} />} onSignedOut={onSignedOut} />
          </div>
        )}
      </div>
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

function NotFound({ back = "/dashboard" }) {
  return (
    <div className="card not-found">
      <h2>Not found</h2>
      <p>This page does not exist or is not available to you.</p>
      <button type="button" onClick={() => navigate(back)}>
        {back === "/dashboard" ? "Back to the dashboard" : "Back to the list"}
      </button>
    </div>
  );
}

const REFRESH_MS = 30000;

function checkedTime() {
  return new Date().toISOString().replace(/\.\d+Z$/, "Z");
}

// Whether the server answered the last request any page made, and when. Every page header shows it,
// so moving between pages keeps the last known state instead of each page knowing its own.
let connection = null;
const connectionListeners = new Set();

function reportConnection(next) {
  connection = next;
  connectionListeners.forEach((listener) => listener());
}

function subscribeToConnection(listener) {
  connectionListeners.add(listener);
  return () => connectionListeners.delete(listener);
}

function Loaded({ token, path, loading, parse, draw, onSignedOut, reloads = 0, header = null, back = "/dashboard" }) {
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
    const fail = (kind) => {
      const checked = checkedTime();
      if (kind === "disconnected") reportConnection({ online: false, checked });
      setState((previous) => ({ kind, data: previous.data, checked }));
    };
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
        else {
          const checked = checkedTime();
          reportConnection({ online: true, checked });
          setState({ kind: "ready", data, checked });
        }
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
  const body = (() => {
  if (state.kind === "notfound") {
    return <NotFound back={back} />;
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
        <Fragment key="data">{draw(state.data)}</Fragment>
      </>
    );
  }
  return (
    <>
      <div className="banner error-banner" role="alert">
        <Icon name="alert" size={18} />
        {state.kind === "disconnected" ? <p>Disconnected. Last checked <Moment at={state.checked} /></p> : <p>This page could not be read just now.</p>}
        {state.data !== null ? <Badge kind="freshness-outdated" /> : null}
        <button type="button" onClick={() => setAttempt(attempt + 1)}>
          Try again
        </button>
      </div>
      {state.data !== null ? <Fragment key="data">{draw(state.data)}</Fragment> : null}
    </>
  );
  })();
  return header !== null ? <>{header(state)}{body}</> : body;
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

// The project last opened in this tab, so the Dashboard can mark it.
let lastProject = null;

function CardStat({ icon, value, label }) {
  return (
    <div className="card-stat">
      <Icon name={icon} size={16} />
      <span className="card-stat-value">{value}</span>
      <span className="card-stat-label">{label}</span>
    </div>
  );
}

function ProjectCard({ project, selected = false }) {
  return (
    <li className={selected ? "project card selected" : "project card"} data-selected={selected ? project.id : undefined}>
      <button type="button" onClick={() => navigate("/projects/" + encodeURIComponent(project.id) + "/features")}>
        <Icon name="folder" />
        {project.name}
        <Icon name="chevronRight" />
      </button>
      {project.available ? (
        <div className="card-stats">
          <CardStat icon="flag" value={project.features} label="Features" />
          <CardStat icon="play" value={project.active_runs} label="Active runs" />
          <CardStat icon="doc" value={project.reviews_waiting} label="Reviews waiting" />
        </div>
      ) : (
        <em>Unavailable</em>
      )}
    </li>
  );
}

function Stat({ icon, tone, value, label }) {
  return (
    <div className="stat">
      <span className={"stat-icon " + tone}><Icon name={icon} size={24} /></span>
      <span className="stat-text">
        <span className="stat-value">{value}</span>
        <span className="stat-label">{label}</span>
      </span>
    </div>
  );
}

// The totals above the project cards; a project that could not be read adds nothing to the sums.
function Dashboard({ projects }) {
  const updated = useMemo(checkedTime, [projects]);
  if (projects.length === 0) {
    return <p>No projects found.</p>;
  }
  const sum = (key) => projects.reduce((total, project) => total + (project.available && Number.isInteger(project[key]) ? project[key] : 0), 0);
  return (
    <>
      <div className="stats">
        <Stat icon="layers" tone="tone-accent" value={projects.length} label="Projects" />
        <Stat icon="play" tone="tone-accent" value={sum("active_runs")} label="Active runs" />
        <Stat icon="doc" tone="tone-failed" value={sum("reviews_waiting")} label="Reviews waiting" />
        <Stat icon="clock" tone="tone-waiting" value={<Moment at={updated} />} label="Last updated" />
      </div>
      <h2 className="section-title">Projects</h2>
      <ul className="projects">
        {projects.map((project) => (
          <ProjectCard key={project.id} project={project} selected={project.id === lastProject} />
        ))}
      </ul>
    </>
  );
}

function refreshButton(onRefresh) {
  return onRefresh ? (
    <button type="button" className="icon-button" data-refresh aria-label="Refresh" title="Refresh" onClick={onRefresh}>
      <Icon name="refresh" />
    </button>
  ) : null;
}

// The right side of every page's first row: whether the server answered the last request, and when
// it last did. Nothing is shown until the first answer (or failure) after signing in.
function ConnectionStatus() {
  const known = useSyncExternalStore(subscribeToConnection, () => connection);
  if (known === null) return null;
  const when = new Date(known.checked);
  return (
    <>
      <p className={known.online ? "connection" : "connection offline"}>
        <span className="dot" aria-hidden="true" />
        {known.online ? "Connected" : "Disconnected"}
      </p>
      <span className="last-updated" title={when.toLocaleString()}>
        Last updated: <time dateTime={known.checked}>{when.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</time>
      </span>
    </>
  );
}

function PageHeader({ title, subtitle, onRefresh, badge = null, back = null }) {
  return (
    <header className="page-header">
      <div className={back === null ? undefined : "has-back"}>
        {back !== null ? (
          <button type="button" className="back-arrow" data-back="page" onClick={() => navigate(back.path)}>
            <Icon name="arrowLeft" size={22} />
            <span className="sr-only">{back.label}</span>
          </button>
        ) : null}
        {badge === null ? <h1>{title}</h1> : <div className="page-title"><h1>{title}</h1>{badge}</div>}
        <p className="subtitle">{subtitle}</p>
      </div>
      <div className="header-status">
        <ConnectionStatus />
        {refreshButton(onRefresh)}
      </div>
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
        <div key={key} className="settings-row">
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
    <section className="card settings-card passkeys">
      <SettingsHead icon="passkey">Passkeys</SettingsHead>
      {failed && (
        <>
          <p role="alert">The passkeys could not be listed.</p>
          <button type="button" onClick={load}>
            Try again
          </button>
        </>
      )}
      <ul className="settings-fields">
        {(failed ? [] : keys).map((key) => (
          <li key={key.id} className="settings-row">
            <strong>{key.label}</strong> <code>{key.id}</code>
          </li>
        ))}
      </ul>
      {passkeysAvailable() && <EnrolPasskey token={null} another onDone={load} />}
    </section>
  );
}

// The audit: every entry is a readable line or a damaged one; anything else is not drawn.
function parseAudit(data) {
  const lines = data && data.audit;
  const ok = Array.isArray(lines) && lines.every((line) => line !== null && typeof line === "object" && (line.damaged === true || (Number.isFinite(line.time) && typeof line.identity === "string" && typeof line.action === "string" && typeof line.project === "string" && typeof line.outcome === "string")));
  return ok ? lines : null;
}

// The entry's time as the page draws times, or null for a damaged entry or a time outside what a date can hold.
function auditTime(line) {
  if (line.damaged === true) return null;
  const when = new Date(line.time * 1000);
  return Number.isNaN(when.getTime()) ? null : when.toISOString().replace(".000Z", "Z");
}

function AuditView({ lines }) {
  if (lines.length === 0) return <section className="card"><p>No actions recorded yet.</p></section>;
  return (
    <section className="card">
      <ul className="audit-lines">
        {lines.map((line, index) => auditTime(line) === null ? (
          <li key={index}>Unreadable entry</li>
        ) : (
          <li key={index} data-outcome={line.outcome}>
            <Moment at={auditTime(line)} />
            <span>{line.identity}</span>
            <strong>{line.action}</strong>
            <span>{line.project}</span>
            {typeof line.run === "string" ? <span>{line.run}</span> : null}
            <span>{line.outcome}</span>
            {typeof line.reason === "string" ? <span>{line.reason}</span> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

// A settings card's title row: a line icon beside a bold title.
function SettingsHead({ icon, children }) {
  return (
    <h2 className="settings-card-head">
      <Icon name={icon} size={18} />
      {children}
    </h2>
  );
}

const ACTIONS_OFF_QUESTION = "Turn remote actions off? Start, Pause, Resume and the other actions disappear until they are turned on again.";
const ACTIONS_ON_QUESTION = "Turn remote actions on? Your passkey confirms it.";

// The switch under the Remote actions row. Off asks first and sends; on asks first, then asks the
// passkey for a fresh check and sends its answer. Only a passkey session can turn them on; signed in
// with the token the row says so instead. Every change carries the version the settings were read at.
function RemoteActionsSwitch({ on, version, token, onSignedOut, onChanged }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  if (typeof version !== "string") return null;
  const session = token === SESSION;
  const send = async () => {
    setPhase("sending");
    setNote("");
    setConflict(false);
    let text = "The change could not be made.";
    try {
      const body = { on: !on, version };
      if (!on) {
        const assertion = await confirmWithPasskey("remote_actions.on");
        if (assertion === null) {
          setNote("The passkey check was cancelled.");
          setPhase("idle");
          return;
        }
        if (typeof assertion.status === "number") {
          if (assertion.status === 401) {
            onSignedOut();
            return;
          }
          setNote("The passkey check could not start.");
          setPhase("idle");
          return;
        }
        body.assertion = assertion;
      }
      const response = await actionRequest(API_SETTINGS + "/remote-actions", token, body);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.ok) {
        // The settings are read again; the switch stays closed until they arrive.
        onChanged();
        return;
      }
      let reply = null;
      try {
        reply = await response.json();
      } catch (unreadable) {
        reply = null;
      }
      if (reply !== null && typeof reply.error === "string") text = reply.error;
      setConflict(response.status === 409);
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const reload = () => {
    setNote("");
    setConflict(false);
    onChanged();
  };
  return (
    <div className="settings-row settings-switch">
      {on || session ? null : <span>Turning remote actions on needs a passkey sign-in.</span>}
      {!on && session ? <span>Turning on asks for your passkey.</span> : null}
      {phase === "idle" && on ? <button type="button" data-actions-off onClick={() => { setNote(""); setPhase("asking"); }}>Turn off</button> : null}
      {phase === "idle" && !on && session ? <button type="button" data-actions-on onClick={() => { setNote(""); setPhase("asking"); }}>Turn on</button> : null}
      {phase === "asking" ? (
        <span role="group" aria-label={on ? ACTIONS_OFF_QUESTION : ACTIONS_ON_QUESTION}>
          <span>{on ? ACTIONS_OFF_QUESTION : ACTIONS_ON_QUESTION}</span>
          <button type="button" data-confirm onClick={send}>{on ? "Yes, turn off" : "Yes, turn on"}</button>
          <button type="button" data-cancel onClick={() => setPhase("idle")}>Cancel</button>
        </span>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
      {conflict ? <button type="button" data-settings-reload onClick={reload}>Reload</button> : null}
    </div>
  );
}

// The options as the server lists them: a harness and the models it allows, nothing else drawn.
function parseOptions(data) {
  const options = data && data.options;
  return Array.isArray(options) && options.every((option) => option !== null && typeof option === "object" && typeof option.harness === "string" && isTextList(option.models)) ? options.filter((option) => option.models.length > 0) : null;
}

// What an editor starts on: the role's own harness and model when they are offered, else the first.
function startingChoice(options, agent) {
  const own = options.find((option) => agent && option.harness === agent.harness) || options[0];
  return { harness: own.harness, model: agent && own.models.includes(agent.model) ? agent.model : own.models[0] };
}

// Edit under an Implementer or Reviewer card: reads the options, offers a harness and a model from
// them, and Save asks before it sends the choice with the version the settings were read at.
function AgentEditor({ role, agent, version, token, onSignedOut, onChanged }) {
  const [phase, setPhase] = useState("idle");
  const [options, setOptions] = useState([]);
  const [choice, setChoice] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  if (typeof version !== "string") return null;
  const edit = async () => {
    setPhase("loading");
    setNote("");
    setConflict(false);
    try {
      const response = await get(API_SETTINGS + "/options", token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const listed = response.ok ? parseOptions(await response.json()) : null;
      if (listed === null || listed.length === 0) {
        setNote("There are no harnesses to choose from.");
        setPhase("idle");
        return;
      }
      setOptions(listed);
      setChoice(startingChoice(listed, agent));
      setPhase("editing");
    } catch (unreachable) {
      setNote("The server could not be reached.");
      setPhase("idle");
    }
  };
  const pickHarness = (harness) => {
    const option = options.find((each) => each.harness === harness);
    if (option) setChoice({ harness, model: option.models[0] });
  };
  const send = async () => {
    setPhase("sending");
    let text = "The change could not be made.";
    try {
      const response = await actionRequest(API_SETTINGS + "/agents", token, { [role]: choice, version });
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.ok) {
        onChanged();
        return;
      }
      let reply = null;
      try {
        reply = await response.json();
      } catch (unreadable) {
        reply = null;
      }
      if (reply !== null && typeof reply.error === "string") text = reply.error;
      setConflict(response.status === 409);
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const question = choice === null ? "" : "Save " + role + ": " + choice.harness + " " + choice.model + "?";
  const current = options.find((option) => choice !== null && option.harness === choice.harness);
  return (
    <div className="settings-row settings-switch">
      {phase === "idle" ? <button type="button" data-agent-edit={role} onClick={edit}>Edit</button> : null}
      {phase === "loading" ? <span role="status">Loading the options…</span> : null}
      {phase === "editing" ? (
        <span role="group" aria-label={"Change the " + role}>
          <label>Harness <select data-agent-harness value={choice.harness} onChange={(event) => pickHarness(event.target.value)}>{options.map((option) => <option key={option.harness} value={option.harness}>{option.harness}</option>)}</select></label>
          <label>Model <select data-agent-model value={choice.model} onChange={(event) => setChoice({ harness: choice.harness, model: event.target.value })}>{current.models.map((model) => <option key={model} value={model}>{model}</option>)}</select></label>
          <button type="button" data-agent-save onClick={() => setPhase("asking")}>Save</button>
          <button type="button" data-cancel onClick={() => setPhase("idle")}>Cancel</button>
        </span>
      ) : null}
      {phase === "asking" ? (
        <span role="group" aria-label={question}>
          <span>{question}</span>
          <span className="muted">Runs already going keep their model.</span>
          <button type="button" data-confirm onClick={send}>Yes, save</button>
          <button type="button" data-cancel onClick={() => setPhase("editing")}>Cancel</button>
        </span>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
      {conflict ? <button type="button" data-settings-reload onClick={() => { setNote(""); setConflict(false); onChanged(); }}>Reload</button> : null}
    </div>
  );
}

function SettingsView({ settings, token, onSignedOut, onChanged }) {
  const on = settings.remote_actions === true;
  return (
    <>
      <section className="card settings-card">
        <SettingsHead icon="sliders">Run settings</SettingsHead>
        <dl className="settings-fields">
          <div className="settings-row" data-remote-actions={on ? "on" : "off"}>
            <dt>Remote actions</dt>
            <dd><span className={"pill " + (on ? "pill-on" : "pill-off")}>{on ? "On" : "Off"}</span></dd>
          </div>
        </dl>
        <RemoteActionsSwitch key={String(on) + " " + settings.version} on={on} version={settings.version} token={token} onSignedOut={onSignedOut} onChanged={onChanged} />
      </section>
      {[["Implementer", "implementer"], ["Reviewer", "reviewer"]].map(([title, role]) => (
        <section key={role} className="card settings-card">
          <SettingsHead icon="cpu">{title}</SettingsHead>
          <AgentFields agent={settings[role]} />
          {on ? <AgentEditor key={settings.version} role={role} agent={settings[role]} version={settings.version} token={token} onSignedOut={onSignedOut} onChanged={onChanged} /> : null}
        </section>
      ))}
    </>
  );
}

const FEATURE_GROUPS = ["draft", "pending", "in_progress", "completed", "abandoned"];

function isFeature(feature) {
  return feature !== null && typeof feature === "object" && typeof feature.name === "string" && typeof feature.status === "string" && (feature.description === null || typeof feature.description === "string") && isTextList(feature.depends_on);
}

function parseFeatures(data) {
  return data && Array.isArray(data.features) && data.features.every(isFeature) ? { rows: data.features, actions: data.remote_actions === true } : null;
}

// A feature's dependencies: a link to the feature's own page for each one in this ledger, a marker
// for one that is not.
function Dependencies({ names, known, hrefFor }) {
  return (
    <ul className="dependencies">
      {names.map((name) => (
        <li key={name}>
          {known.has(name) ? (
            <a href={hrefFor(name)} data-dependency={name} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(hrefFor(name)); } }}>{name}</a>
          ) : (
            <span className="missing-dependency">{name}<em>{" — Missing dependency"}</em></span>
          )}
        </li>
      ))}
    </ul>
  );
}

// The only three things the page can ask the server to do. Each is a button that asks first, naming
// what it will touch; nothing is sent until it is confirmed. The answer is the server's reason when it
// refuses, and a refused sign-in goes back to the token screen.
function controlPath(control, tail) {
  return "/api/projects/" + encodeURIComponent(control.projectId) + "/" + tail;
}

async function actionRequest(path, token, body) {
  const headers = { "Content-Type": "application/json" };
  if (token === SESSION) {
    // Only a passkey session carries a CSRF token; a bearer token goes straight out.
    const status = await authStatus();
    if (status !== null && typeof status.csrf === "string") headers["X-CSRF-Token"] = status.csrf;
  } else {
    headers.Authorization = "Bearer " + token;
  }
  return fetch(path, { method: "POST", headers, body: JSON.stringify(body === undefined ? {} : body), cache: "no-store", referrerPolicy: "no-referrer" });
}

function RunAction({ kind, label, question, path, token, onSignedOut, onDone, done }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const send = async () => {
    setPhase("sending");
    let text = "The action could not be completed.";
    try {
      const response = await actionRequest(path, token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      let body = null;
      try {
        body = await response.json();
      } catch (unreadable) {
        body = null;
      }
      if (response.ok) {
        // Done for good: the control stays closed while the list is read again, so the same action cannot be sent twice.
        setNote(done(body !== null && typeof body.run === "string" ? body.run : ""));
        setPhase("done");
        onDone();
        return;
      } else if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  return (
    <span className="run-action">
      {phase === "idle" ? <button type="button" {...{ ["data-" + kind]: "" }} onClick={() => { setNote(""); setPhase("asking"); }}>{label}</button> : null}
      {phase === "asking" ? (
        <span role="group" aria-label={question}>
          <span>{question}</span>
          <button type="button" data-confirm onClick={send}>{"Yes, " + label.toLowerCase()}</button>
          <button type="button" data-cancel onClick={() => setPhase("idle")}>Cancel</button>
        </span>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
    </span>
  );
}

function featureHref(projectId, name) {
  return "/projects/" + encodeURIComponent(projectId) + "/features/" + encodeURIComponent(name);
}

function whenOf(text) {
  const at = typeof text === "string" ? Date.parse(text) : NaN;
  return Number.isNaN(at) ? 0 : at;
}

// What the project's runs say about a feature: the stage of its latest run, and when it last moved
// (the latest run activity or the ledger entry, whichever is newer).
function featureFacts(feature, runs) {
  const mine = runs.filter((run) => run.feature === feature.name).sort((a, b) => whenOf(b.updated_at ?? b.started_at) - whenOf(a.updated_at ?? a.started_at));
  const latest = mine.length > 0 ? mine[0] : null;
  const times = [feature.recorded_at ?? null, latest === null ? null : latest.updated_at ?? latest.started_at].filter((time) => whenOf(time) > 0);
  const updated = times.length === 0 ? null : times.reduce((best, time) => (whenOf(time) > whenOf(best) ? time : best));
  return { stage: latest === null || !latest.stage ? null : latest.stage, updated, runs: mine };
}

// The project's runs give one feature its stage and run history (read only on its own page). Reading them is a courtesy: when they cannot be
// read the features still show, just without a stage.
function useFeatureRuns(token, projectId, reloads, enabled) {
  const [runs, setRuns] = useState([]);
  useEffect(() => {
    if (!enabled) return undefined;
    let current = true;
    const controller = new AbortController();
    (async () => {
      try {
        const response = await get("/api/projects/" + encodeURIComponent(projectId) + "/runs", token, controller.signal);
        if (!current || !response.ok) return;
        const parsed = parseOrchestrations(await response.json());
        if (current) setRuns(parsed === null ? [] : parsed.rows);
      } catch (unreadable) {
        // no stages, nothing more
      }
    })();
    return () => {
      current = false;
      controller.abort();
    };
  }, [token, projectId, reloads, enabled]);
  return runs;
}

// The project chooser at the top of the page; choosing one opens that project's same section.
function ProjectSelect({ projects, projectId, section = "features", onOpen = null }) {
  const [open, setOpen] = useState(false);
  const box = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") setOpen(false);
    };
    const onPress = (event) => {
      if (box.current && !box.current.contains(event.target)) setOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPress);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPress);
    };
  }, [open]);
  if ((projects === null || projects.length === 0) && onOpen === null) return null;
  const known = projectId === null || projects.some((project) => project.id === projectId);
  const choices = [{ id: null, name: "All projects" }, ...(known ? [] : [{ id: projectId, name: projectId }]), ...projects];
  const current = choices.find((project) => project.id === projectId);
  return (
    <div className="project-picker" ref={box}>
      <span className="project-switch-prefix">Project:</span>
      <button type="button" className="project-switcher" data-project-select aria-label="Project" aria-haspopup="listbox" aria-expanded={open ? "true" : "false"} onClick={() => { if (!open && onOpen !== null) onOpen(); setOpen(!open); }}>
        <span className="project-switcher-text">{current.name}</span>
        <Icon name="chevronDown" size={18} />
      </button>
      <ul className="project-list" role="listbox" aria-label="Project" hidden={!open}>
        {choices.map((project) => (
          <li key={project.id ?? ""} role="presentation">
            <button type="button" role="option" tabIndex={-1} data-project-choice={project.id ?? ""} aria-selected={project.id === projectId ? "true" : "false"} onClick={() => { setOpen(false); navigate(project.id === null ? "/" + section : "/projects/" + encodeURIComponent(project.id) + "/" + section); }}>{project.name}</button>
          </li>
        ))}
      </ul>
    </div>
  );
}


function FeatureState({ status }) {
  const icon = { draft: "doc", pending: "clock", in_progress: "play", completed: "check", abandoned: "close" }[status] ?? "flag";
  return <span className={"feature-state state-" + (FEATURE_GROUPS.includes(status) ? status : "other")}><Icon name={icon} size={14} />{status}</span>;
}

// Each feature is a card that opens its own page. Search, state, stage and sort narrow the list;
// the newest activity comes first.
const FEATURES_PER_PAGE = 10;

// Previous and Next under a list longer than one page.
function Pager({ at, pages, onPage }) {
  if (pages <= 1) return null;
  return (
    <nav className="pager" aria-label="Pagination">
      <button type="button" data-page-prev disabled={at === 0} onClick={() => onPage(at - 1)}>Previous</button>
      <span className="pager-status">{"Page " + (at + 1) + " of " + pages}</span>
      <button type="button" data-page-next disabled={at === pages - 1} onClick={() => onPage(at + 1)}>Next</button>
    </nav>
  );
}

function FeatureList({ features, showProject = false }) {
  const [query, setQuery] = useState("");
  const [state, setState] = useState("all");
  const [page, setPage] = useState(0);
  if (features.length === 0) {
    return <p>No features in this project's ledger.</p>;
  }
  const states = [...FEATURE_GROUPS, ...[...new Set(features.map((feature) => feature.status))].filter((status) => !FEATURE_GROUPS.includes(status))];
  const needle = query.trim().toLowerCase();
  const shown = features.filter((feature) => (state === "all" || feature.status === state) && feature.name.toLowerCase().includes(needle));
  const pages = Math.max(1, Math.ceil(shown.length / FEATURES_PER_PAGE));
  const at = Math.min(page, pages - 1);
  const visible = shown.slice(at * FEATURES_PER_PAGE, (at + 1) * FEATURES_PER_PAGE);
  return (
    <>
      <div className="feature-filters">
        <label className="search-box">
          <Icon name="search" size={18} />
          <input type="search" data-feature-search aria-label="Search features" placeholder="Search features" autoComplete="off" spellCheck={false} value={query} onChange={(event) => { setQuery(event.target.value); setPage(0); }} />
        </label>
        <select data-feature-status aria-label="State" value={state} onChange={(event) => { setState(event.target.value); setPage(0); }}>
          <option value="all">State: All</option>
          {states.map((name) => (
            <option key={name} value={name}>{name}</option>
          ))}
        </select>
      </div>
      {shown.length === 0 ? <p>No features match the filters.</p> : null}
      {shown.length > 0 ? (
        <div className="feature-table-wrap">
          <table className="feature-table">
            <thead>
              <tr><th scope="col">Feature <span className="feature-count" data-feature-count>{shown.length}</span></th><th scope="col">State</th></tr>
            </thead>
            <tbody>
              {visible.map((feature) => (
                <tr key={feature.projectId + "/" + feature.name} className="feature">
                  <td>
                    <a className="feature-link" href={featureHref(feature.projectId, feature.name)} data-feature-open={feature.name} title={showProject ? feature.projectName : undefined} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(featureHref(feature.projectId, feature.name)); } }}>{feature.name}</a>
                  </td>
                  <td><FeatureState status={feature.status} /></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      <Pager at={at} pages={pages} onPage={setPage} />
    </>
  );
}

// One feature: its status, description, dependencies (each a link to that feature), the runs made
// for it, and the action to start one when it is pending.
const FEATURE_TABS = [["description", "Description"], ["test", "Test file"], ["dependencies", "Dependencies"], ["runs", "Runs"]];

function FeatureDetail({ feature, features, runs, projectId, control }) {
  const [tab, setTab] = useState("description");
  const known = new Set(features.map((entry) => entry.name));
  const facts = featureFacts(feature, runs);
  const listPath = "/projects/" + encodeURIComponent(projectId) + "/features";
  return (
    <>
      <section className="card feature-detail">
        <div className="feature-detail-head">
          <button type="button" className="back-arrow" data-back="features" aria-label="Back to features" onClick={() => navigate(listPath)}><Icon name="arrowLeft" size={22} /></button>
          <div className="feature-detail-title">
            <div className="feature-detail-line">
              <h2>{feature.name}</h2>
              <FeatureState status={feature.status} />
            </div>
            <p className="feature-detail-sub">{[...(validTime(facts.updated) === null ? [] : [<Fragment key="u">{"Updated "}<Moment at={facts.updated} /></Fragment>]), ...(validTime(feature.recorded_at ?? null) === null ? [] : [<Fragment key="r">{"Recorded "}<Moment at={feature.recorded_at} /></Fragment>])].flatMap((part, index) => (index === 0 ? [part] : [" · ", part]))}</p>
          </div>
        </div>
        {control !== null && feature.status === "pending" ? <RunAction kind="start" label="Start" question={"Start " + feature.name + "?"} path={controlPath(control, "features/" + encodeURIComponent(feature.name) + "/runs")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(run) => (run === "" ? "Started." : "Started run " + run + ".")} /> : null}
        <div className="feature-tabs" role="tablist" aria-label="Feature details">
          {FEATURE_TABS.map(([id, label]) => (
            <button key={id} type="button" role="tab" id={"feature-tab-" + id} aria-selected={tab === id ? "true" : "false"} aria-controls="feature-panel" data-feature-tab={id} onClick={() => setTab(id)}>{label}</button>
          ))}
        </div>
        <div className="feature-panel" id="feature-panel" role="tabpanel" aria-labelledby={"feature-tab-" + tab}>
          {tab === "description" ? (feature.description ? <p>{feature.description}</p> : <p className="muted">No description.</p>) : null}
          {tab === "test" ? (feature.test_file ? <p><code data-test-file>{feature.test_file}</code></p> : <p className="muted">No test file recorded.</p>) : null}
          {tab === "runs" ? (
          facts.runs.length === 0 ? <p>No runs for this feature yet.</p> : (
            <ul>
              {facts.runs.map((run) => (
                <li key={run.run_id} className="run">
                  <span className="run-id">{run.run_id}</span>
                  <RunState state={run.state} />
                  {run.stage ? <span>{"Stage: " + run.stage}</span> : null}
                  {RUN_ID.test(run.run_id) ? <button type="button" data-open={run.run_id} onClick={() => navigate("/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(run.run_id))}>Open timeline</button> : null}
                </li>
              ))}
            </ul>
          )
          ) : null}
          {tab === "dependencies" ? (feature.depends_on.length === 0 ? <p>No dependencies.</p> : <Dependencies names={feature.depends_on} known={known} hrefFor={(name) => featureHref(projectId, name)} />) : null}
        </div>
      </section>
    </>
  );
}

// Features carry the project they belong to, so one table can hold several projects' ledgers.
function tagged(rows, projectId, projectName) {
  return rows.map((feature) => ({ ...feature, projectId, projectName }));
}

// Every readable project's ledger, read side by side and merged into one list.
// Reads one list from every readable project side by side; toRows turns a project's parsed reply
// into rows, and a project that cannot be read is counted rather than shown.
function useEveryProject({ token, projects, reloads, onSignedOut, part, parse, toRows }) {
  const [state, setState] = useState({ loading: true, rows: [], unread: 0 });
  const readable = projects.filter((project) => project.available !== false);
  const key = readable.map((project) => project.id).join("\n");
  useEffect(() => {
    let current = true;
    const controller = new AbortController();
    (async () => {
      const results = await Promise.all(readable.map(async (project) => {
        try {
          const response = await get("/api/projects/" + encodeURIComponent(project.id) + "/" + part, token, controller.signal);
          if (response.status === 401) return "signed-out";
          if (!response.ok) return null;
          const parsed = parse(await response.json());
          return parsed === null ? null : toRows(parsed, project);
        } catch (unreadable) {
          return null;
        }
      }));
      if (!current) return;
      if (results.includes("signed-out")) {
        onSignedOut();
        return;
      }
      setState({ loading: false, rows: results.filter((rows) => rows !== null).flat(), unread: results.filter((rows) => rows === null).length });
    })();
    return () => {
      current = false;
      controller.abort();
    };
  }, [token, key, reloads]);
  return state;
}

function AllFeatures({ token, projects, reloads, onSignedOut }) {
  const state = useEveryProject({ token, projects, reloads, onSignedOut, part: "features", parse: parseFeatures, toRows: (parsed, project) => tagged(parsed.rows, project.id, project.name) });
  if (state.loading) {
    return (
      <div className="skeleton" role="status" aria-busy="true">
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="sr-only">Loading the features…</span>
      </div>
    );
  }
  return (
    <>
      {state.unread > 0 ? <p className="banner error-banner" role="alert"><Icon name="alert" size={18} />{state.unread === 1 ? "One project could not be read." : state.unread + " projects could not be read."}</p> : null}
      <FeatureList features={state.rows} showProject />
    </>
  );
}

// The Features pages of one project: the list, or one feature's details when its name is given.
function FeaturesPage({ token, onSignedOut, projectId, projects, name, reloads, onRefresh }) {
  const runs = useFeatureRuns(token, projectId, reloads, name !== null);
  const featuresPath = "/api/projects/" + encodeURIComponent(projectId) + "/features";
  const control = (data) => (data.actions ? { token, projectId, onSignedOut, onDone: onRefresh } : null);
  return (
    <>
      <Loaded key={featuresPath} reloads={reloads} header={(state) => (
        <>
          <PageHeader title={name === null ? "Features" : "Feature"} subtitle={name === null ? "The project's feature ledger" : name} onRefresh={onRefresh} />
          <ProjectSelect projects={projects} projectId={projectId} />
        </>
      )} token={token} path={featuresPath} loading="Loading the features…" parse={parseFeatures} onSignedOut={onSignedOut} draw={(data) => {
        if (name === null) return <FeatureList features={tagged(data.rows, projectId, projectId)} />;
        const feature = data.rows.find((entry) => entry.name === name);
        return feature === undefined ? <NotFound /> : <FeatureDetail feature={feature} features={data.rows} runs={runs} projectId={projectId} control={control(data)} />;
      }} />
    </>
  );
}

const RUN_STATES = { running: "Running", completed: "Completed", failed: "Failed", blocked: "Blocked", needs_review: "Needs review", pause_requested: "Pause requested", paused: "Paused", merged: "Merged" };

function isOrchestration(run) {
  const text = (value) => value === null || typeof value === "string";
  return run !== null && typeof run === "object" && typeof run.run_id === "string" && typeof run.feature === "string" && typeof run.state === "string" && text(run.stage) && text(run.started_at) && text(run.updated_at) && text(run.target_branch);
}

function parseOrchestrations(data) {
  return data && Array.isArray(data.runs) && data.runs.every(isOrchestration) ? { rows: data.runs, actions: data.remote_actions === true } : null;
}

// The states a run can be in, needs-attention first, as the state filter offers them.
const STATE_ORDER = ["running", "pause_requested", "paused", "needs_review", "blocked", "failed", "completed", "merged"];

// By when each run last moved (its update, else its start), newest first or oldest first. The sort
// is stable, so runs with the same time keep the server's order.
function byUpdated(runs, order) {
  const when = (run) => whenOf(run.updated_at ?? run.started_at ?? null);
  return [...runs].sort((a, b) => (order === "asc" ? when(a) - when(b) : when(b) - when(a)));
}

// A run's state as a coloured pill. A state the page does not know is "Unknown", never a success.
const RUN_ICONS = { running: "play", completed: "check", merged: "check", failed: "close", blocked: "close", needs_review: "doc", pause_requested: "clock", paused: "clock" };

function RunState({ state }) {
  const known = Object.hasOwn(RUN_STATES, state);
  return <span className={"run-state feature-state run-" + (known ? state : "unknown")}><Icon name={known ? RUN_ICONS[state] : "flag"} size={14} />{known ? RUN_STATES[state] : "Unknown"}</span>;
}

// The pill beside the Orchestrations title: how many of the listed runs are running now.
function runningBadge(rows) {
  if (!Array.isArray(rows)) return null;
  const running = rows.filter((run) => run.state === "running").length;
  return <span className="feature-state run-running running-badge" data-running-count><span className="dot" aria-hidden="true" />{running + " running"}</span>;
}

function runHref(projectId, runId) {
  return "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(runId);
}

// The runs as a table, the server's order (newest first) or grouped by state when asked. Search,
// state and stage narrow it; the count beside the Run header follows them.
const RUNS_PER_PAGE = 10;

function OrchestrationList({ runs, control = null, projectId = null, showProject = false }) {
  const [query, setQuery] = useState("");
  const [state, setState] = useState("all");
  const [stage, setStage] = useState("all");
  const [sort, setSort] = useState("desc");
  const [page, setPage] = useState(0);
  if (runs.length === 0) {
    return <p>{showProject ? "No runs in any project yet." : "No runs in this project yet."}</p>;
  }
  const projectOf = (run) => run.projectId ?? projectId;
  const controlOf = (run) => (run.control === undefined ? control : run.control);
  const known = (run) => Object.hasOwn(RUN_STATES, run.state);
  const states = STATE_ORDER;
  const stages = [...new Set(runs.map((run) => run.stage).filter((name) => name))];
  const needle = query.trim().toLowerCase();
  const matches = runs.filter((run) => (state === "all" || (state === "unknown" ? !known(run) : run.state === state)) && (stage === "all" || run.stage === stage) && (run.feature.toLowerCase().includes(needle) || run.run_id.toLowerCase().includes(needle)));
  const shown = byUpdated(matches, sort);
  const pages = Math.max(1, Math.ceil(shown.length / RUNS_PER_PAGE));
  const at = Math.min(page, pages - 1);
  const visible = shown.slice(at * RUNS_PER_PAGE, (at + 1) * RUNS_PER_PAGE);
  const choose = (set) => (event) => {
    set(event.target.value);
    setPage(0);
  };
  const actions = runs.some((run) => controlOf(run) !== null && (run.state === "running" || run.state === "paused"));
  return (
    <>
      <div className="feature-filters run-filters">
        <label className="search-box">
          <Icon name="search" size={18} />
          <input type="search" data-run-search aria-label="Search runs" placeholder="Search runs" autoComplete="off" spellCheck={false} value={query} onChange={choose(setQuery)} />
        </label>
        <select data-run-state aria-label="State" value={state} onChange={choose(setState)}>
          <option value="all">State: All</option>
          {states.map((name) => <option key={name} value={name}>{RUN_STATES[name]}</option>)}
          {runs.some((run) => !known(run)) ? <option value="unknown">Unknown</option> : null}
        </select>
        <select data-run-stage aria-label="Stage" value={stage} onChange={choose(setStage)}>
          <option value="all">Stage: All</option>
          {stages.map((name) => <option key={name} value={name}>{name}</option>)}
        </select>
        <button type="button" className="sort-toggle" data-run-sort={sort} aria-label={sort === "desc" ? "Sort by updated: newest first" : "Sort by updated: oldest first"} title={sort === "desc" ? "Newest first" : "Oldest first"} onClick={() => {
          setSort(sort === "desc" ? "asc" : "desc");
          setPage(0);
        }}>
          <Icon name={sort === "desc" ? "sortDesc" : "sortAsc"} size={18} />
          <span aria-hidden="true">Updated</span>
        </button>
      </div>
      {shown.length === 0 ? <p>No runs match the filters.</p> : (
        <div className="feature-table-wrap">
          <table className="feature-table run-table">
            <thead>
              <tr>
                <th scope="col">Run <span className="feature-count" data-run-count>{shown.length}</span></th>
                <th scope="col">State</th>
                <th scope="col">Stage</th>
                <th scope="col" aria-sort={sort === "desc" ? "descending" : "ascending"}>Updated</th>
                {actions ? <th scope="col"><span className="sr-only">Actions</span></th> : null}
              </tr>
            </thead>
            <tbody>
              {visible.map((run) => (
                <tr key={projectOf(run) + "/" + run.run_id} className="run-row">
                  <td>
                    {RUN_ID.test(run.run_id) ? <button type="button" className="feature-link run-open" data-open={run.run_id} onClick={() => navigate(runHref(projectOf(run), run.run_id))}>{run.feature}</button> : <span className="run-open">{run.feature}</span>}
                    <span className="run-id">{run.run_id + (run.target_branch ? " into " + run.target_branch : "") + (showProject ? " · " + run.projectName : "")}</span>
                  </td>
                  <td><RunState state={run.state} /></td>
                  <td>{run.stage ? <span className="run-stage">{run.stage}</span> : <span className="muted" aria-label="No stage">–</span>}</td>
                  <td><Moment at={run.updated_at} /></td>
                  {actions ? (
                    <td>
                      <RowActions run={run} control={controlOf(run)} />
                    </td>
                  ) : null}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <Pager at={at} pages={pages} onPage={setPage} />
    </>
  );
}

// Pause on a running row, Resume on a paused one, when the server allows actions.
function RowActions({ run, control }) {
  if (control === null || !RUN_ID.test(run.run_id)) return null;
  if (run.state === "running") return <RunAction kind="pause" label="Pause" question={"Pause " + run.run_id + "?"} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/pause")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Pause requested."} />;
  if (run.state === "paused") return <RunAction kind="resume" label="Resume" question={"Resume " + run.run_id + "?"} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/resume")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next) => (next === "" ? "Resumed." : "Resumed as run " + next + ".")} />;
  return null;
}

// Every readable project's runs, read side by side and merged into one list, newest first. The
// rows are handed up so the header can count the running ones.
function AllOrchestrations({ token, projects, reloads, onSignedOut, onRefresh, onRows }) {
  const toRows = (parsed, project) => {
    const control = parsed.actions ? { token, projectId: project.id, onSignedOut, onDone: onRefresh } : null;
    return parsed.rows.map((run) => ({ ...run, projectId: project.id, projectName: project.name, control }));
  };
  const state = useEveryProject({ token, projects, reloads, onSignedOut, part: "runs", parse: parseOrchestrations, toRows });
  const when = (run) => whenOf(run.updated_at ?? null);
  const rows = useMemo(() => [...state.rows].sort((a, b) => when(b) - when(a)), [state.rows]);
  useEffect(() => {
    if (!state.loading) onRows(rows);
  }, [state.loading, rows]);
  if (state.loading) {
    return (
      <div className="skeleton" role="status" aria-busy="true">
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="sr-only">Loading the runs…</span>
      </div>
    );
  }
  return (
    <>
      {state.unread > 0 ? <p className="banner error-banner" role="alert"><Icon name="alert" size={18} />{state.unread === 1 ? "One project could not be read." : state.unread + " projects could not be read."}</p> : null}
      <OrchestrationList runs={rows} showProject />
    </>
  );
}

// The Orchestrations page across every project: the header, the project picker, and the merged runs.
function AllOrchestrationsPage({ token, reloads, onRefresh, onSignedOut }) {
  const [rows, setRows] = useState(null);
  return (
    <Loaded key={API_PROJECTS} reloads={reloads} header={(state) => <PageHeader title="Orchestrations" subtitle="Runs and pipeline executions across every project" badge={runningBadge(rows)} onRefresh={onRefresh} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => (
      <>
        <ProjectSelect projects={projects} projectId={null} section="orchestrations" />
        <AllOrchestrations token={token} projects={projects} reloads={reloads} onSignedOut={onSignedOut} onRefresh={onRefresh} onRows={setRows} />
      </>
    )} onSignedOut={onSignedOut} />
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

function EventItem({ event }) {
  return (
    <li className="event">
      <strong className="event-kind">{event.kind}</strong>
      {event.stage ? <span className="event-meta">{"Stage: " + event.stage}</span> : null}
      <span className="event-meta">{"Attempt " + event.attempt}</span>
      <Moment at={event.occurred_at} />
    </li>
  );
}

// Consecutive events of one stage and attempt become one group, so a retry is its own group and a
// later success never hides an earlier failure. A stage start always opens a new group, and a
// review group is named by its run-wide round (the server's `review_round`, else counted on).
function groupEvents(events, seed = 0) {
  const items = [];
  // Events with no stage seen since the last group: they join it if the same attempt carries on
  // after them, so an attempt is never split in two; otherwise they stand on their own.
  let between = [];
  let round = seed;
  for (const event of events) {
    if (!event.stage) {
      between.push(event);
      continue;
    }
    const review = event.stage === "review";
    if (review) round = Number.isInteger(event.review_round) ? event.review_round : event.kind === "stage.started" || round === 0 ? round + 1 : round;
    const last = items[items.length - 1];
    const starts = event.kind === "stage.started";
    if (last && last.events !== undefined && last.stage === event.stage && last.attempt === event.attempt && !starts && (!review || last.round === round)) {
      last.events.push(...between, event);
      if (event.kind.endsWith(".failed")) last.failed = true;
    } else {
      for (const loose of between) items.push({ event: loose });
      items.push({
        stage: event.stage,
        attempt: event.attempt,
        title: review ? "Review round " + round : event.stage + ", attempt " + event.attempt,
        failed: event.kind.endsWith(".failed"),
        round: review ? round : 0,
        events: [event],
      });
    }
    between = [];
  }
  for (const loose of between) items.push({ event: loose });
  return items;
}

// A step's mark: failed, finished, or still open.
function StepIcon({ failed, done }) {
  const kind = failed ? "failed" : done ? "done" : "open";
  return <span className={"step-icon step-" + kind} aria-hidden="true"><Icon name={{ failed: "close", done: "check", open: "clock" }[kind]} size={16} /></span>;
}

// How long a step took, from its first event to its last, when both times are readable.
function stepDuration(events) {
  const first = validTime(events[0].occurred_at);
  const last = validTime(events[events.length - 1].occurred_at);
  if (first === null || last === null) return null;
  const seconds = Math.round((last - first) / 1000);
  if (seconds < 1) return null;
  return seconds < 60 ? seconds + "s" : seconds < 3600 ? Math.floor(seconds / 60) + "m " + (seconds % 60) + "s" : Math.floor(seconds / 3600) + "h " + Math.floor((seconds % 3600) / 60) + "m";
}

function StepTime({ events }) {
  const text = stepDuration(events);
  return text === null ? null : <span className="attempt-time">{text}</span>;
}

// The events as a list, drawn the same way for the fetched page and for events that arrive live.
function EventTimeline({ events, rounds = 0 }) {
  return (
    <ol className="run-events">
      {groupEvents(events, rounds).map((item) =>
        item.events === undefined ? (
          <EventItem key={item.event.event_id} event={item.event} />
        ) : (
          <li key={item.events[0].event_id} className="attempt" data-failed={item.failed ? "true" : undefined}>
            <div className="attempt-head">
              <StepIcon failed={item.failed} done={item.events.some((event) => event.kind.endsWith(".completed"))} />
              <h3 className="attempt-title">{item.title}</h3>
              <StepTime events={item.events} />
            </div>
            {item.failed ? <strong className="attempt-outcome">Failed</strong> : null}
            <ol className="attempt-events">
              {item.events.map((event) => (
                <EventItem key={event.event_id} event={event} />
              ))}
            </ol>
          </li>
        )
      )}
    </ol>
  );
}

const STEP_STATES = { done: "Done", failed: "Failed", current: "In progress", open: "Not finished" };

// The run's stages as one line of steps: each finished, failed, or still going, with how long it took.
// A stage passed is done even without its own completion event, since the run moved on from it.
function RunSteps({ events, rounds, running, onLogs }) {
  const steps = groupEvents(events, rounds).filter((item) => item.events !== undefined);
  return (
    <ol className="run-steps">
      {steps.map((step, at) => {
        const last = at === steps.length - 1;
        const kind = step.failed ? "failed" : !last || step.events.some((event) => event.kind.endsWith(".completed")) ? "done" : running ? "current" : "open";
        const took = stepDuration(step.events);
        const name = step.round > 0 ? step.title : step.attempt > 1 ? step.stage + " · attempt " + step.attempt : step.stage;
        return (
          <li key={step.events[0].event_id} className="run-step" data-step={kind}>
            <span className="run-step-mark" aria-hidden="true">{kind === "done" ? <Icon name="check" size={14} /> : kind === "failed" ? <Icon name="close" size={14} /> : null}</span>
            <span className="run-step-name">{name}</span>
            <span className="sr-only">{STEP_STATES[kind]}</span>
            <span className="run-step-time" aria-hidden={kind === "current" ? "true" : undefined}>{kind === "current" ? "In progress" + (took === null ? "" : " (" + took + ")") : took}</span>
            {kind === "failed" ? (
              <p className="run-step-failure">
                <Icon name="alert" size={14} />
                <span>{"Failed: " + step.events[step.events.length - 1].kind + " "}<Moment at={step.events[step.events.length - 1].occurred_at} /></span>
                {onLogs ? <button type="button" className="link-button" data-step-logs onClick={onLogs}>See the logs</button> : null}
              </p>
            ) : null}
          </li>
        );
      })}
    </ol>
  );
}

const RUN_TABS = [["timeline", "Timeline"], ["logs", "Logs"], ["events", "Events"]];

// One run: its header, then three tabs that switch in place: its steps, its logs, and every event.
// The events are fetched a page at a time; the next page is its own address, so every request stays
// one plain GET. The logs are fetched the first time their tab opens, and kept while it is hidden.
function RunTimeline({ timeline, listPath, runPath, follow, logs, startTab = "timeline" }) {
  const { run, events, next_cursor: next } = timeline;
  const [tab, setTab] = useState(startTab);
  const [logsOpened, setLogsOpened] = useState(startTab === "logs");
  const rounds = timeline.review_rounds;
  const live = next === null && follow !== null;
  const choose = (id) => {
    setTab(id);
    if (id === "logs") setLogsOpened(true);
  };
  const panels = (all) => (
    <>
      <div className="feature-panel" id="run-panel-timeline" role="tabpanel" aria-labelledby="run-tab-timeline" hidden={tab !== "timeline"}>
        {all.length === 0 ? <p>No events recorded.</p> : <RunSteps events={all} rounds={rounds} running={run.state === "running"} onLogs={() => choose("logs")} />}
      </div>
      <div className="feature-panel" id="run-panel-events" role="tabpanel" aria-labelledby="run-tab-events" hidden={tab !== "events"}>
        {all.length === 0 ? null : <EventTimeline events={all} rounds={rounds} />}
      </div>
    </>
  );
  return (
    <section className="card feature-detail run-detail">
      <div className="feature-detail-head">
        <button type="button" className="back-arrow" data-back="list" aria-label="Back to the runs" onClick={() => navigate(listPath)}><Icon name="arrowLeft" size={22} /></button>
        <div className="feature-detail-title">
          <div className="feature-detail-line">
            <h2>{run.feature}</h2>
            <RunState state={run.state} />
          </div>
          {validTime(run.started_at) === null ? null : (
            <p className="feature-detail-sub run-detail-sub">
              <span>Started <Moment at={run.started_at} /></span>
            </p>
          )}
        </div>
      </div>
      <div className="feature-tabs" role="tablist" aria-label="Run details">
        {RUN_TABS.map(([id, label]) => (
          <button key={id} type="button" role="tab" id={"run-tab-" + id} aria-selected={tab === id ? "true" : "false"} aria-controls={"run-panel-" + id} data-run-tab={id} onClick={() => choose(id)}>{label}</button>
        ))}
      </div>
      {live ? <FollowLive key={follow.base} rounds={rounds} token={follow.token} base={follow.base} from={events.length > 0 ? events[events.length - 1].event_id : follow.cursor} shown={events} onSignedOut={follow.onSignedOut} draw={panels} quiet={tab === "logs"} /> : panels(events)}
      {next !== null && tab !== "logs" && <button type="button" data-more="events" onClick={() => navigate(runPath + "/after/" + encodeURIComponent(next))}>Load more events</button>}
      {logsOpened && (
        <div className="feature-panel" id="run-panel-logs" role="tabpanel" aria-labelledby="run-tab-logs" hidden={tab !== "logs"}>
          <RunLogs {...logs} runId={run.run_id} />
        </div>
      )}
    </section>
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
function FollowLive({ token, base, from, shown, rounds, onSignedOut, draw, quiet = false }) {
  const [more, setMore] = useState([]);
  const [status, setStatus] = useState("live");
  const [retry, setRetry] = useState(0);
  const [gap, setGap] = useState(false);
  const last = useRef(from);
  // Set while the run is being read again from its start because the place to resume from is gone.
  const restarting = useRef(false);
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
        if (response.status === 404 && last.current !== null) {
          // The event to resume after is no longer in the run: read it from the start, and say so.
          last.current = null;
          restarting.current = true;
          setRetry((count) => count + 1);
          return;
        }
        if (response.status === 404 || response.status === 403) {
          restarting.current = false;
          setStatus("stopped");
          return;
        }
        if (!response.ok || !response.body) {
          again();
          return;
        }
        setStatus("live");
        if (restarting.current) {
          restarting.current = false;
          setGap(true);
        }
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
            if (event === null) continue;
            // The place to resume from moves with every event read, shown already or not.
            last.current = event.event_id;
            if (ids.current.has(event.event_id)) continue;
            ids.current.add(event.event_id);
            fresh.push(event);
          }
          if (fresh.length > 0) setMore((known) => [...known, ...fresh]);
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
      {draw([...shown, ...more.filter((event) => !have.has(event.event_id))])}
      {gap && <p className="stream-gap" role="status" hidden={quiet}>Some events may be missing</p>}
      <p className={status === "live" ? "live-status sr-only" : "live-status"} data-live hidden={quiet}>{status === "live" ? "Following live" : status === "stopped" ? "Live updates stopped" : "Reconnecting…"}</p>
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

// The run's logs inside its Logs tab: the file list is read once, then one file's page at a time.
function RunLogs({ token, onSignedOut, reloads, base, file, cursor, runId }) {
  return <Loaded key={base} token={token} path={base} loading="Loading the logs…" parse={parseLogFiles} draw={(files) => <LogsPanel files={files} token={token} onSignedOut={onSignedOut} reloads={reloads} base={base} wanted={file} cursor={cursor} runId={runId} />} onSignedOut={onSignedOut} />;
}

// One file's lines with its filters, follow switch and pager. The filters stay while another page of
// the same file opens; choosing another file starts it unfiltered on its first page. Paging keeps the
// cursors already passed, so Prev goes back without asking the server where it was.
function LogsPanel({ files, token, onSignedOut, reloads, base, wanted, cursor, runId }) {
  const openable = files.filter((file) => LOG_FILE.test(file.name));
  const [name, setName] = useState(wanted !== null ? wanted : openable.length > 0 ? openable[0].name : null);
  const [pages, setPages] = useState([wanted !== null ? cursor : null]);
  const [text, setText] = useState("");
  const [level, setLevel] = useState("all");
  const [follow, setFollow] = useState(true);
  const filters = { text, setText, level, setLevel };
  const pick = (next) => {
    setName(next);
    setPages([null]);
    setText("");
    setLevel("all");
  };
  const at = pages[pages.length - 1];
  const pagePath = name === null ? null : base + "/" + encodeURIComponent(name) + (at === null ? "" : "/after/" + encodeURIComponent(at));
  const pager = { number: pages.length, onPrev: pages.length > 1 ? () => setPages(pages.slice(0, -1)) : null, onNext: (next) => setPages([...pages, next]) };
  return (
    <div className="run-logs">
      <div className="run-logs-head">
        <h3>Logs</h3>
        <button type="button" role="switch" className="follow-switch" data-log-follow aria-checked={follow ? "true" : "false"} onClick={() => setFollow(!follow)}>
          <span>Follow</span>
          <span className="switch-track" aria-hidden="true"><span className="switch-thumb" /></span>
        </button>
      </div>
      {openable.length > 1 ? (
        <select className="log-file-select" data-log-file aria-label="Log file" value={name} onChange={(event) => pick(event.target.value)}>
          {openable.map((file) => (
            <option key={file.name} value={file.name}>{file.name + " · " + file.bytes + " bytes"}</option>
          ))}
        </select>
      ) : name !== null ? <p className="log-file-name">{name}</p> : null}
      {name === null ? <p>No logs for this run.</p> : <Loaded key={pagePath} reloads={reloads} token={token} path={pagePath} loading="Loading the log…" parse={parseLogPage} draw={(page) => <LogPage page={page} filters={filters} follow={follow} onFollow={setFollow} pager={pager} />} onSignedOut={onSignedOut} />}
    </div>
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

// How near the bottom still counts as at the bottom, in pixels.
const BOTTOM_SLACK = 24;
// A time a line starts with, drawn apart from the rest of the line.
const LOG_TIME = /^(\[?(?:\d{4}-\d\d-\d\d[T ])?\d\d:\d\d:\d\d(?:[.,]\d+)?(?:Z|[+-]\d\d:?\d\d)?\]?)(?=\s)/;

function LogLine({ line }) {
  const time = LOG_TIME.exec(line);
  return (
    <div className="log-line" data-level={lineLevel(line)}>
      {time === null ? line : <><span className="log-time">{time[1]}</span>{line.slice(time[1].length)}</>}
    </div>
  );
}

// The lines, which follow new ones while Follow is on. Scrolling up turns it off, and the lines that
// arrived since are counted beside a control that jumps to the latest; reaching the bottom turns it on.
function LogLines({ shown, total, follow = true, onFollow = () => {} }) {
  const box = useRef(null);
  const atBottom = useRef(true);
  const first = useRef(true);
  const mounted = useRef(false);
  const pinned = useRef(null);
  const [seen, setSeen] = useState(total);
  const onScroll = () => {
    const el = box.current;
    // The browser moves a scroller whose content shrank and says so with a scroll event; that is the
    // content moving, not the viewer, so it must not change whether they are following.
    if (pinned.current === el.scrollTop) return;
    pinned.current = null;
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_SLACK;
    onFollow(atBottom.current);
    if (atBottom.current) setSeen(total);
  };
  const toLatest = () => {
    const el = box.current;
    el.scrollTop = el.scrollHeight;
    atBottom.current = true;
    onFollow(true);
    setSeen(total);
  };
  useLayoutEffect(() => {
    if (total < seen) setSeen(total);
    if (first.current) {
      // A log opened taller than its box starts at the top, so it is not following.
      first.current = false;
      const el = box.current;
      if (!follow) atBottom.current = false;
      else if (el.scrollHeight > el.clientHeight) atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_SLACK;
      onFollow(atBottom.current);
      return;
    }
    if (atBottom.current) {
      box.current.scrollTop = box.current.scrollHeight;
      setSeen(total);
    } else {
      // Reading the offset settles any clamp now, so the scroll event that follows can be told apart.
      pinned.current = box.current.scrollTop;
    }
  }, [shown.length, total]);
  // The Follow switch: on jumps to the latest line, off stops following where the viewer is.
  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      return;
    }
    if (follow && !atBottom.current) toLatest();
    if (!follow) atBottom.current = false;
  }, [follow]);
  const waiting = Math.max(0, total - seen);
  return (
    <>
      <pre className="log-lines" ref={box} onScroll={onScroll}>
        {shown.map((line, at) => (
          <LogLine key={at} line={line} />
        ))}
      </pre>
      {waiting > 0 && (
        <p className="new-lines" role="status">
          <span>{waiting + (waiting === 1 ? " new line" : " new lines")}</span>
          <button type="button" data-jump-latest onClick={toLatest}>Jump to latest</button>
        </p>
      )}
    </>
  );
}

// One page of a log's already-redacted lines, drawn as plain text, with its filters above and its pager below.
function LogPage({ page, filters: { text, setText, level, setLevel }, follow, onFollow, pager: { number, onPrev, onNext } }) {
  const { lines, next_cursor: next, truncated } = page;
  const shown = filterLines(lines, text, level);
  return (
    <>
      <div className="log-tools">
        <label className="search-box">
          <Icon name="search" size={16} />
          <input type="search" data-log-filter aria-label="Filter lines" placeholder="Filter logs (text)" autoComplete="off" spellCheck={false} value={text} onChange={(event) => setText(event.target.value)} />
        </label>
        <select data-log-level aria-label="Level" value={level} onChange={(event) => setLevel(event.target.value)}>
          <option value="all">All levels</option>
          {LOG_LEVELS.map((name) => (
            <option key={name} value={name}>{name}</option>
          ))}
        </select>
      </div>
      {lines.length === 0 ? <p>This log has no more lines.</p> : null}
      {lines.length > 0 && shown.length === 0 ? <p>No lines match the filter.</p> : null}
      <LogLines shown={shown} total={lines.length} follow={follow} onFollow={onFollow} />
      <div className="log-pager">
        <button type="button" data-log-prev disabled={onPrev === null} onClick={onPrev ?? undefined}><Icon name="chevronLeft" size={16} />Prev</button>
        <span className="log-page">{"Page " + number}</span>
        <button type="button" data-more="lines" disabled={next === null} onClick={() => onNext(next)}>Next<Icon name="chevronRight" size={16} /></button>
        <span className="log-count">{shown.length + " of " + lines.length + " lines"}</span>
        <button type="button" className="log-download" data-log-download disabled={shown.length === 0} onClick={() => saveLines(LOG_FILE.test(page.name) ? page.name : "log.log", shown)}><Icon name="download" size={16} />Download</button>
      </div>
      {truncated && next === null ? <p>The log was cut short: only its first part is shown.</p> : null}
    </>
  );
}

// What a project's address shows. Only the Reviews tab is built: its list and reports come from
// the project's own endpoints and stay under the project's address.
function ProjectPage({ token, onSignedOut, projectId, projects, path: address, reloads, onRefresh }) {
  const features = /^\/projects\/[^/]+(?:\/features(?:\/([^/]+))?)?$/.exec(address);
  if (features !== null) {
    let name = null;
    try {
      name = features[1] === undefined ? null : decodeURIComponent(features[1]);
    } catch (malformed) {
      return <NotFound />;
    }
    return <FeaturesPage token={token} onSignedOut={onSignedOut} projectId={projectId} projects={projects} name={name} reloads={reloads} onRefresh={onRefresh} />;
  }
  if (/^\/projects\/[^/]+\/orchestrations$/.test(address)) {
    const runsPath = "/api/projects/" + encodeURIComponent(projectId) + "/runs";
    return (
      <>
        <Loaded key={runsPath} reloads={reloads} header={(state) => (
          <>
            <PageHeader title="Orchestrations" subtitle="Runs and pipeline executions for this project" badge={runningBadge(state.data?.rows)} onRefresh={onRefresh} />
            <ProjectSelect projects={projects} projectId={projectId} section="orchestrations" />
          </>
        )} token={token} path={runsPath} loading="Loading the runs…" parse={parseOrchestrations} draw={(data) => <OrchestrationList runs={data.rows} projectId={projectId} control={data.actions ? { token, projectId, onSignedOut, onDone: onRefresh } : null} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  // A run's page. Its tabs switch in place; a logs address (kept so older links still work) opens the
  // same page on its Logs tab, at that file and line if it names them.
  const logs = /^\/projects\/[^/]+\/orchestrations\/([^/]+)\/logs(?:\/([^/]+)(?:\/after\/([^/]+))?)?$/.exec(address);
  const timeline = logs !== null ? null : /^\/projects\/[^/]+\/orchestrations\/([^/]+)(?:\/after\/([^/]+))?$/.exec(address);
  if (logs !== null || timeline !== null) {
    let run = null;
    let cursor = null;
    let file = null;
    let line = null;
    try {
      if (logs !== null) {
        run = decodeURIComponent(logs[1]);
        file = logs[2] === undefined ? null : decodeURIComponent(logs[2]);
        line = logs[3] === undefined ? null : decodeURIComponent(logs[3]);
      } else {
        run = decodeURIComponent(timeline[1]);
        cursor = timeline[2] === undefined ? null : decodeURIComponent(timeline[2]);
      }
    } catch (malformed) {
      return <NotFound />;
    }
    if (!RUN_ID.test(run) || (cursor !== null && !RUN_ID.test(cursor)) || (file !== null && !LOG_FILE.test(file)) || (line !== null && !LINE_CURSOR.test(line))) return <NotFound />;
    const listPath = "/projects/" + encodeURIComponent(projectId) + "/orchestrations";
    const runPath = listPath + "/" + encodeURIComponent(run);
    const apiRun = "/api/projects/" + encodeURIComponent(projectId) + "/runs/" + encodeURIComponent(run);
    const timelinePath = apiRun + (cursor === null ? "" : "/after/" + encodeURIComponent(cursor));
    const logsAt = { token, onSignedOut, reloads, base: apiRun + "/logs", file, cursor: line };
    return (
      <>
        <Loaded key={timelinePath} reloads={reloads} header={(state) => (
          <>
            <PageHeader title="Run" subtitle={run} onRefresh={onRefresh} />
            <ProjectSelect projects={projects} projectId={projectId} section="orchestrations" />
          </>
        )} token={token} path={timelinePath} loading="Loading the run…" parse={parseTimeline} draw={(data) => <RunTimeline key={address} timeline={data} listPath={listPath} runPath={runPath} startTab={logs !== null ? "logs" : "timeline"} logs={logsAt} follow={{ token, onSignedOut, cursor, base: apiRun }} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  const match = /^\/projects\/[^/]+\/reviews(?:\/([^/]*))?$/.exec(address);
  if (match === null) return <p>This page is not available yet.</p>;
  if (match[1] !== undefined && !RUN_ID.test(match[1])) return <NotFound />;
  return <ReviewsPage token={token} onSignedOut={onSignedOut} reloads={reloads} onRefresh={onRefresh} api={"/api/projects/" + encodeURIComponent(projectId) + "/reviews"} listPath={"/projects/" + encodeURIComponent(projectId) + "/reviews"} open={match[1]} projectId={projectId} projects={projects} />;
}

function SignedIn({ token, onSignedOut, onSignOut = null, projectId = null, projects = null }) {
  const [reloads, setReloads] = useState(0);
  const hash = useSyncExternalStore(subscribeToLocation, currentPath);
  if (projectId !== null && projects !== null && !projects.some((project) => project.id === projectId)) {
    return <NotFound />;
  }
  if (projectId !== null) {
    return <ProjectPage token={token} onSignedOut={onSignedOut} projectId={projectId} projects={projects} path={hash} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} />;
  }
  if (UNBUILT_DESTINATIONS.some((prefix) => hash === prefix || hash.startsWith(prefix + "/"))) {
    return <p>This page is not available yet.</p>;
  }
  if (hash === "/settings/audit") {
    return (
      <>
        <PageHeader title="Audit" subtitle="Recent remote actions, read only" back={{ path: "/settings", label: "Back to settings" }} onRefresh={() => setReloads(reloads + 1)} />
        <Loaded key={API_AUDIT} reloads={reloads} token={token} path={API_AUDIT} loading="Loading the audit…" parse={parseAudit} draw={(lines) => <AuditView lines={lines} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  if (hash.startsWith("/settings/")) {
    return <NotFound />;
  }
  if (hash === "/settings") {
    return (
      <>
        <PageHeader title="Settings" subtitle="Global run settings" onRefresh={() => setReloads(reloads + 1)} />
        <div className="settings-page">
          {/* The passkeys are read before the settings; their card is placed lower by the stylesheet. */}
          <PasskeysCard />
          <Loaded key={API_SETTINGS} reloads={reloads} token={token} path={API_SETTINGS} loading="Loading the settings…" parse={parseSettings} draw={(settings) => <SettingsView settings={settings} token={token} onSignedOut={onSignedOut} onChanged={() => setReloads(reloads + 1)} />} onSignedOut={onSignedOut} />
          <section className="card settings-card settings-audit">
            <SettingsHead icon="doc">Activity</SettingsHead>
            <a className="settings-link" href="/settings/audit" onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate("/settings/audit"); } }}>
              Audit
              <span className="muted">Recent remote actions</span>
              <Icon name="chevronRight" size={18} />
            </a>
          </section>
          <section className="card settings-card settings-appearance">
            <SettingsHead icon="sun">Appearance</SettingsHead>
            <div className="settings-row">
              <span>Theme</span>
              <ThemeChoice />
            </div>
          </section>
          {onSignOut && (
            <section className="card settings-card settings-account">
              <SettingsHead icon="logout">Account</SettingsHead>
              <div className="settings-row">
                <span>Sign out of this browser</span>
                <button type="button" className="danger-button" data-sign-out onClick={onSignOut}>
                  <Icon name="logout" size={16} />
                  Sign out
                </button>
              </div>
            </section>
          )}
        </div>
      </>
    );
  }
  if (hash === "/dashboard" || hash === "/") {
    return (
      <>
        <Loaded key={API_PROJECTS} reloads={reloads} header={(state) => <PageHeader title="Dashboard" subtitle="Monitor Orq projects and orchestration activity" onRefresh={() => setReloads(reloads + 1)} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => <Dashboard projects={projects} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  const everywhere = { "/features": ["Features", "Every project's feature ledger", "features", "Loading the features…", parseFeatures], "/orchestrations": ["Orchestrations", "Runs and pipeline executions across every project", "runs", "Loading the runs…", parseOrchestrations] }[hash];
  if (hash === "/orchestrations") {
    return <AllOrchestrationsPage token={token} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} onSignedOut={onSignedOut} />;
  }
  if (everywhere !== undefined) {
    const [title, subtitle, part, loading, parse] = everywhere;
    const onRefresh = () => setReloads(reloads + 1);
    return (
      <>
        <Loaded key={API_PROJECTS} reloads={reloads} header={(state) => <PageHeader title={title} subtitle={subtitle} onRefresh={onRefresh} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => part === "features" ? (
          <>
            <ProjectSelect projects={projects} projectId={null} />
            <AllFeatures token={token} projects={projects} reloads={reloads} onSignedOut={onSignedOut} />
          </>
        ) : (
          <>
            {projects.map((project) => {
              const id = encodeURIComponent(project.id);
              const base = "/api/projects/" + id + "/" + part;
              return (
                <section key={project.id} className="project-section">
                  <h2>{project.name}</h2>
                  <Loaded key={base} reloads={reloads} token={token} path={base} loading={loading} parse={parse} onSignedOut={onSignedOut} draw={(data) => {
                    return <OrchestrationList runs={data.rows} projectId={project.id} control={data.actions ? { token, projectId: project.id, onSignedOut, onDone: onRefresh } : null} />;
                  }} />
                </section>
              );
            })}
          </>
        )} onSignedOut={onSignedOut} />
      </>
    );
  }
  const review = /^\/reviews(?:\/([^/]*))?$/.exec(hash);
  if (review !== null && (review[1] === undefined || RUN_ID.test(review[1]))) {
    return <ReviewsPage token={token} onSignedOut={onSignedOut} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} api={API_REVIEWS} listPath="/reviews" open={review[1]} showProject />;
  }
  return <NotFound />;
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
  // Outside a project the sidebar dropdown loads the list the first time it is used, so a page
  // that never opens it asks for nothing extra.
  const [menuProjects, setMenuProjects] = useState(null);
  const menuAsked = useRef(false);
  const openSwitcher = () => {
    if (inProject || menuAsked.current) return;
    menuAsked.current = true;
    (async () => {
      try {
        const response = await get(API_PROJECTS, token, undefined);
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        setMenuProjects(response.ok ? parseProjects(await response.json()) : null);
      } catch (failure) {
        menuAsked.current = false;
      }
    })();
  };
  const known = projects !== null && projects.some((project) => project.id === named);
  useEffect(() => {
    if (known) lastProject = named;
  }, [known, named]);
  return (
    <Shell projectId={projects === null || known ? named : null} projects={inProject ? projects : menuProjects} onOpenSwitcher={openSwitcher} onSignOut={onSignOut}>
      <SignedIn token={token} onSignedOut={onSignedOut} onSignOut={onSignOut} projectId={named} projects={projects} />
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
    reportConnection(null);
    setToken(null);
    setMessage(SIGN_IN_FAILED);
    refresh();
  };
  const signOut = () => {
    reportConnection(null);
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
openRootAtDashboard();
createRoot(document.getElementById("app")).render(<App />);
