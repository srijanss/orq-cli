// The review inbox page. The bearer token is typed in, held in one piece of state and sent as a
// header on each request. A copy is kept in this tab's sessionStorage only, so a refresh does not
// ask again and closing the tab forgets it; it goes nowhere else (no address, no other storage).
// Every string from the API is rendered as text.
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { Shell, usePageRefresh, usePhone } from "./shell.jsx";
import { Badge, BADGES } from "./badge.jsx";
import { Icon } from "./icons.jsx";
import { ThemeChoice, ThemeToggle } from "./theme.jsx";
import { readPins, writePins } from "./pins.js";
import { authStatus, confirmWithPasskey, endAllSessions, listPasskeys, passkeyBlocker, passkeysAvailable, registerPasskey, removePasskey, renamePasskey, signInWithPasskey, signOutOfSession } from "./passkey.js";
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

// A count that opens what it counts: a button to its page.
function CardStat({ icon, value, label, to, id }) {
  return (
    <button type="button" className="card-stat" data-card-stat={id} onClick={() => navigate(to)}>
      <Icon name={icon} size={16} />
      <span className="card-stat-value">{value}</span>
      <span className="card-stat-label">{label}</span>
    </button>
  );
}

function ProjectCard({ project, selected = false, pinned = false, onPin = null }) {
  const base = "/projects/" + encodeURIComponent(project.id) + "/";
  return (
    <li className={selected ? "project card selected" : "project card"} data-selected={selected ? project.id : undefined}>
      <button type="button" onClick={() => navigate("/projects/" + encodeURIComponent(project.id) + "/features")}>
        <Icon name="folder" />
        {project.name}
        <Icon name="chevronRight" />
      </button>
      {onPin !== null ? (
        <button type="button" className="pin-button" data-pin={project.id} aria-pressed={pinned ? "true" : "false"} aria-label={(pinned ? "Unpin " : "Pin ") + project.name} title={pinned ? "Unpin" : "Pin to the top"} onClick={onPin}>
          <Icon name="pin" size={18} />
        </button>
      ) : null}
      {project.available ? (
        <div className="card-stats">
          <CardStat icon="flag" value={project.features} label="Features" to={base + "features"} id={project.id + "/features"} />
          <CardStat icon="play" value={project.active_runs} label="Active runs" to={base + "orchestrations"} id={project.id + "/runs"} />
          <CardStat icon="doc" value={project.reviews_waiting} label="Reviews waiting" to={base + "reviews"} id={project.id + "/reviews"} />
        </div>
      ) : (
        <em>Unavailable</em>
      )}
    </li>
  );
}

// A total; given a page (`to`), it is a button that opens it.
function Stat({ icon, tone, value, label, to = null, link = undefined }) {
  const inside = (
    <>
      <span className={"stat-icon " + tone}><Icon name={icon} size={24} /></span>
      <span className="stat-text">
        <span className="stat-value">{value}</span>
        <span className="stat-label">{label}</span>
      </span>
    </>
  );
  return to === null ? (
    <div className="stat">{inside}</div>
  ) : (
    <button type="button" className="stat stat-link" data-stat-link={link} onClick={() => navigate(to)}>{inside}</button>
  );
}

// The totals above the project cards; a project that could not be read adds nothing to the sums.
function Dashboard({ projects }) {
  const updated = useMemo(checkedTime, [projects]);
  const [pins, setPins] = useState(readPins);
  if (projects.length === 0) {
    return <p>No projects found.</p>;
  }
  const sum = (key) => projects.reduce((total, project) => total + (project.available && Number.isInteger(project[key]) ? project[key] : 0), 0);
  const toggle = (id) => {
    const next = pins.includes(id) ? pins.filter((pin) => pin !== id) : [...pins, id];
    writePins(next);
    setPins(next);
  };
  const pinned = projects.filter((project) => pins.includes(project.id));
  const rest = projects.filter((project) => !pins.includes(project.id));
  const card = (project) => <ProjectCard key={project.id} project={project} selected={project.id === lastProject} pinned={pins.includes(project.id)} onPin={() => toggle(project.id)} />;
  return (
    <>
      <div className="stats">
        <Stat icon="layers" tone="tone-accent" value={projects.length} label="Projects" />
        <Stat icon="play" tone="tone-accent" value={sum("active_runs")} label="Active runs" to="/orchestrations" link="runs" />
        <Stat icon="doc" tone="tone-failed" value={sum("reviews_waiting")} label="Reviews waiting" to="/reviews" link="reviews" />
        <Stat icon="clock" tone="tone-waiting" value={<Moment at={updated} />} label="Last updated" />
      </div>
      {pinned.length > 0 ? (
        <>
          <h2 className="section-title">Pinned</h2>
          <ul className="projects projects-pinned">{pinned.map(card)}</ul>
        </>
      ) : null}
      {rest.length > 0 ? (
        <>
          <h2 className="section-title">Projects</h2>
          <ul className="projects projects-rest">{rest.map(card)}</ul>
        </>
      ) : null}
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

// On a phone the page's Refresh sits in the top bar instead of here.
function PageHeader({ title, subtitle, onRefresh, badge = null, back = null }) {
  const phone = usePhone();
  usePageRefresh(phone ? onRefresh : null);
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
        {phone ? null : refreshButton(onRefresh)}
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
function AgentFields({ agent, edit = {} }) {
  return (
    <dl className="settings-fields">
      {SETTING_FIELDS.map(([label, key]) => (
        <div key={key} className="settings-row">
          <dt>{label}</dt>
          <dd>{edit[key] !== undefined ? edit[key] : <>{agent && agent[key] !== null ? agent[key] : "—"}{agent && agent[key] !== null ? null : <span className="muted"> (not set)</span>}</>}</dd>
        </div>
      ))}
    </dl>
  );
}

// One passkey on Settings. Rename opens a name field that starts with the current name; Save sends it, and
// a refusal shows the server's reason and keeps the field open. Remove asks first; removing the passkey
// this session signed in with signs the page out.
function PasskeyRow({ passkey, onChanged, onSignOut }) {
  const [label, setLabel] = useState(null);
  const [removing, setRemoving] = useState(false);
  const [sending, setSending] = useState(false);
  const [note, setNote] = useState("");
  const close = () => {
    setLabel(null);
    setRemoving(false);
    setNote("");
  };
  const save = async () => {
    setSending(true);
    const refused = await renamePasskey(passkey.id, label);
    setSending(false);
    if (refused === null) {
      close();
      onChanged();
    } else {
      setNote(refused);
    }
  };
  const remove = async () => {
    setSending(true);
    const { refused, signedOut } = await removePasskey(passkey.id);
    setSending(false);
    if (refused !== null) {
      setRemoving(false);
      setNote(refused);
    } else if (signedOut && onSignOut) {
      onSignOut();
    } else {
      close();
      onChanged();
    }
  };
  if (label === null) {
    const question = "Remove " + passkey.label + "? Devices signed in with it are signed out.";
    return (
      <li className="settings-row">
        <strong>{passkey.label}</strong> {passkey.this_device === true && <span className="badge">This device</span>} <code>{passkey.id}</code>{" "}
        {removing ? (
          <Confirm question={question} answer="Yes, remove" tone="danger" busy={sending} onConfirm={remove} onCancel={close} />
        ) : (
          <>
            <button type="button" data-passkey-rename={passkey.id} onClick={() => { setNote(""); setLabel(passkey.label); }}>
              Rename
            </button>
            <button type="button" className="danger-button" data-passkey-remove={passkey.id} onClick={() => { setNote(""); setRemoving(true); }}>
              Remove
            </button>
          </>
        )}
        {note !== "" && <p role="alert">{note}</p>}
      </li>
    );
  }
  return (
    <li className="settings-row passkey-rename">
      <label>
        Name <input type="text" data-passkey-label maxLength={40} value={label} disabled={sending} onChange={(event) => setLabel(event.target.value)} />
      </label>
      <button type="button" data-passkey-save disabled={sending || label.trim() === ""} onClick={save}>
        Save
      </button>
      <button type="button" data-cancel disabled={sending} onClick={close}>
        Cancel
      </button>
      {note !== "" && <p role="alert">{note}</p>}
    </li>
  );
}

// Settings, signed in with a passkey: the passkeys on the server and a way to add another one.
// Signed in with a passkey, ends every session on every device after asking; the page then signs out.
function SignOutEverywhere({ onSignOut }) {
  const [asking, setAsking] = useState(false);
  const [sending, setSending] = useState(false);
  const [note, setNote] = useState("");
  const question = "Sign out every device? You will need your passkey or the token to sign in again.";
  const endAll = async () => {
    setSending(true);
    const refused = await endAllSessions();
    setSending(false);
    setAsking(false);
    if (refused === null) onSignOut();
    else setNote(refused);
  };
  return (
    <div className="settings-row">
      <span>Sign out every device</span>
      {asking ? (
        <Confirm question={question} answer="Yes, sign out everywhere" tone="danger" busy={sending} onConfirm={endAll} onCancel={() => setAsking(false)} />
      ) : (
        <button type="button" className="danger-button" data-sign-out-everywhere onClick={() => { setNote(""); setAsking(true); }}>
          <Icon name="logout" size={16} />
          Sign out everywhere
        </button>
      )}
      {note !== "" && <p role="alert">{note}</p>}
    </div>
  );
}

function PasskeysCard({ onSignOut = null }) {
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
          <PasskeyRow key={key.id} passkey={key} onChanged={load} onSignOut={onSignOut} />
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
    <section className="card audit-card">
      <ul className="audit-lines">
        {lines.map((line, index) => auditTime(line) === null ? (
          <li key={index} className="audit-entry audit-damaged muted">Unreadable entry</li>
        ) : (
          <li key={index} className="audit-entry" data-outcome={line.outcome}>
            <div className="audit-head">
              <strong className="audit-action">{line.action}</strong>
              <span className={"badge " + (line.outcome === "allowed" ? "badge-status-completed" : line.outcome === "refused" ? "badge-status-failed" : "badge-risk-other")}>{line.outcome}</span>
              <Moment at={auditTime(line)} />
            </div>
            <dl className="audit-meta">
              {[["By", line.identity], ["Project", line.project], ["Run", line.run]].filter(([, value]) => typeof value === "string").map(([label, value]) => (
                <div key={label}>
                  <dt>{label}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
            {typeof line.reason === "string" ? <p className={"audit-reason" + (line.outcome === "refused" ? " audit-reason-refused" : "")}>{line.reason}</p> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

// A settings card's title row: a line icon beside a bold title.
function SettingsHead({ icon, children, action = null }) {
  if (action !== null) {
    return (
      <div className="settings-card-top">
        <SettingsHead icon={icon}>{children}</SettingsHead>
        {action}
      </div>
    );
  }
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
  const can = typeof version === "string";
  const session = token === SESSION;
  // The switch flips only when the page can send the change: never while its question is open.
  const live = can && phase === "idle" && (on || session);
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
  const hint = !can ? null : on ? null : session ? "Turning on asks for your passkey." : "Turning remote actions on needs a passkey sign-in.";
  return (
    <>
      <dl className="settings-fields">
        <div className="settings-row" data-remote-actions={on ? "on" : "off"}>
          <dt>Remote actions</dt>
          <dd className="settings-value">
            <button type="button" role="switch" className="follow-switch" aria-label="Remote actions" aria-checked={on ? "true" : "false"} disabled={!live} data-actions-off={live && on ? "" : undefined} data-actions-on={live && !on ? "" : undefined} onClick={() => { setNote(""); setPhase("asking"); }}>
              <span className="switch-track" aria-hidden="true"><span className="switch-thumb" /></span>
              <span>{on ? "On" : "Off"}</span>
            </button>
          </dd>
        </div>
      </dl>
      {hint !== null ? <p className="settings-hint muted">{hint}</p> : null}
      {phase === "asking" ? (
        <div className="settings-block">
          <Confirm question={on ? ACTIONS_OFF_QUESTION : ACTIONS_ON_QUESTION} answer={on ? "Yes, turn off" : "Yes, turn on"} tone="primary" onConfirm={send} onCancel={() => setPhase("idle")} />
        </div>
      ) : null}
      <SettingsNotes sending={phase === "sending"} note={note} conflict={conflict} onReload={reload} />
    </>
  );
}

// What a settings change says while it is sent and after a refusal, with Reload for a stale version.
function SettingsNotes({ sending, sendingText = "Sending…", note, conflict, onReload }) {
  if (!sending && note === "" && !conflict) return null;
  return (
    <div className="settings-block settings-notes">
      {sending ? <span role="status">{sendingText}</span> : null}
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
      {conflict ? <button type="button" data-settings-reload onClick={onReload}>Reload</button> : null}
    </div>
  );
}

// The options as the server lists them: a harness, the models it allows and, when it has any, the effort
// levels it takes; nothing else drawn.
function parseOptions(data) {
  const options = data && data.options;
  const ok = (option) => option !== null && typeof option === "object" && typeof option.harness === "string" && isTextList(option.models) && (option.efforts === undefined || isTextList(option.efforts));
  return Array.isArray(options) && options.every(ok) ? options.filter((option) => option.models.length > 0) : null;
}

// The effort levels a harness option takes; none when it lists none.
function effortsOf(option) {
  return option && Array.isArray(option.efforts) ? option.efforts : [];
}

// An effort kept for `option`: the level when it takes it, else not set ("").
function keptEffort(option, effort) {
  return typeof effort === "string" && effortsOf(option).includes(effort) ? effort : "";
}

// What an editor starts on: the role's own harness, model and effort when they are offered, else the first.
function startingChoice(options, agent) {
  const own = options.find((option) => agent && option.harness === agent.harness) || options[0];
  return { harness: own.harness, model: agent && own.models.includes(agent.model) ? agent.model : own.models[0], effort: keptEffort(own, agent && agent.effort) };
}

// What Save sends for a choice: the effort (null when not set) only when the harness takes levels, so a
// harness without them leaves the stored effort to the server.
function sentChoice(options, choice) {
  const option = options.find((each) => each.harness === choice.harness);
  const sent = { harness: choice.harness, model: choice.model };
  if (effortsOf(option).length > 0) sent.effort = choice.effort || null;
  return sent;
}

// Edit under an Implementer or Reviewer card: reads the options, offers a harness and a model from
// them, and Save asks before it sends the choice with the version the settings were read at.
function AgentEditor({ title, role, agent, version, token, onSignedOut, onChanged }) {
  const [phase, setPhase] = useState("idle");
  const [options, setOptions] = useState([]);
  const [choice, setChoice] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  if (typeof version !== "string") {
    return (
      <>
        <SettingsHead icon="cpu">{title}</SettingsHead>
        <AgentFields agent={agent} />
      </>
    );
  }
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
    if (option) setChoice({ harness, model: option.models[0], effort: keptEffort(option, choice.effort) });
  };
  const send = async () => {
    setPhase("sending");
    let text = "The change could not be made.";
    try {
      const response = await actionRequest(API_SETTINGS + "/agents", token, { [role]: sentChoice(options, choice), version });
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
  const current = options.find((option) => choice !== null && option.harness === choice.harness);
  const levels = effortsOf(current);
  const question = choice === null ? "" : "Save " + role + ": " + choice.harness + " " + choice.model + (levels.length > 0 ? ", effort " + (choice.effort || "not set") : "") + "?";
  const editing = phase === "editing" || phase === "asking";
  const fields = editing ? {
    harness: <select data-agent-harness aria-label={title + " harness"} value={choice.harness} disabled={phase !== "editing"} onChange={(event) => pickHarness(event.target.value)}>{options.map((option) => <option key={option.harness} value={option.harness}>{option.harness}</option>)}</select>,
    model: <select data-agent-model aria-label={title + " model"} value={choice.model} disabled={phase !== "editing"} onChange={(event) => setChoice({ ...choice, model: event.target.value })}>{current.models.map((model) => <option key={model} value={model}>{model}</option>)}</select>,
  } : {};
  if (editing && levels.length > 0) {
    fields.effort = <select data-agent-effort aria-label={title + " effort"} value={choice.effort} disabled={phase !== "editing"} onChange={(event) => setChoice({ ...choice, effort: event.target.value })}><option value="">Not set</option>{levels.map((level) => <option key={level} value={level}>{level}</option>)}</select>;
  }
  const action = phase === "idle" ? <button type="button" data-agent-edit={role} onClick={edit}><Icon name="pencil" size={16} />Edit</button> : phase === "loading" ? <span role="status" className="muted">Loading the options…</span> : null;
  return (
    <>
      <SettingsHead icon="cpu" action={action}>{title}</SettingsHead>
      <AgentFields agent={agent} edit={fields} />
      {phase === "editing" ? (
        <div className="settings-block settings-actions">
          <button type="button" className="primary-button" data-agent-save onClick={() => setPhase("asking")}><Icon name="check" size={16} />Save</button>
          <button type="button" data-cancel onClick={() => setPhase("idle")}>Cancel</button>
        </div>
      ) : null}
      {phase === "asking" ? (
        <div className="settings-block">
          <Confirm question={question} detail="Runs already going keep their model." answer="Yes, save" tone="primary" onConfirm={send} onCancel={() => setPhase("editing")} />
        </div>
      ) : null}
      <SettingsNotes sending={phase === "sending"} note={note} conflict={conflict} onReload={() => { setNote(""); setConflict(false); onChanged(); }} />
    </>
  );
}

// The Storage card's cleanup, while remote actions are on: "Preview cleanup" reads what a cleanup would
// remove and lists it; "Clean up" asks with the preview's totals and sends its hash, so only the set shown
// is removed. A set that changed since, or a busy prune, is shown in the server's words.
function parsePreview(data) {
  const counts = ["files", "bytes", "runs", "metadata"];
  return data !== null && typeof data === "object" && isTextList(data.lines) && counts.every((key) => Number.isInteger(data[key])) && typeof data.size === "string" && typeof data.hash === "string" ? data : null;
}

function StorageCleanup({ token, onSignedOut }) {
  const [phase, setPhase] = useState("idle");
  const [preview, setPreview] = useState(null);
  const [lines, setLines] = useState([]);
  const [note, setNote] = useState("");
  const reasonOf = async (response, fallback) => {
    try {
      const reply = await response.json();
      return reply !== null && typeof reply.error === "string" ? reply.error : fallback;
    } catch (unreadable) {
      return fallback;
    }
  };
  const look = async () => {
    setPhase("loading");
    setNote("");
    setLines([]);
    setPreview(null);
    try {
      const response = await get(API_SETTINGS + "/storage/preview", token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (!response.ok) {
        setNote(await reasonOf(response, "The cleanup could not be previewed."));
        setPhase("idle");
        return;
      }
      const found = parsePreview(await response.json());
      if (found === null) {
        setNote("The cleanup could not be previewed.");
        setPhase("idle");
        return;
      }
      setPreview(found);
      setPhase("previewed");
    } catch (unreachable) {
      setNote("The server could not be reached.");
      setPhase("idle");
    }
  };
  const send = async () => {
    setPhase("sending");
    try {
      const response = await actionRequest(API_SETTINGS + "/storage/prune", token, { hash: preview.hash });
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.ok) {
        let reply = null;
        try {
          reply = await response.json();
        } catch (unreadable) {
          reply = null;
        }
        setLines(reply !== null && isTextList(reply.lines) ? reply.lines : ["Cleaned up."]);
      } else {
        setNote(await reasonOf(response, "The cleanup could not be done."));
      }
    } catch (unreachable) {
      setNote("The server could not be reached.");
    }
    setPreview(null);
    setPhase("idle");
  };
  const something = preview !== null && preview.files + preview.metadata > 0;
  const question = preview === null ? "" : "Clean up " + preview.files + " file(s), " + preview.size + " across " + preview.runs + " run(s)" + (preview.metadata > 0 ? ", and the metadata of " + preview.metadata + " run(s)" : "") + "?";
  // An empty preview still says so, whether or not the server worded it.
  const listed = preview === null ? [] : something || preview.lines.length > 0 ? preview.lines : ["Nothing to prune."];
  return (
    <>
      <div className="settings-row">
        <span className="settings-label">Run files past their retention</span>
        {phase === "idle" ? <button type="button" data-storage-preview onClick={look}><Icon name="search" size={16} />Preview cleanup</button> : null}
        {phase === "loading" ? <span role="status" className="muted">Reading what a cleanup would remove…</span> : null}
      </div>
      {preview !== null && phase !== "loading" ? (
        <div className="settings-block">
          <ul className="storage-preview">{listed.map((line, index) => <li key={index} data-storage-line>{line}</li>)}</ul>
        </div>
      ) : null}
      {phase === "previewed" ? (
        <div className="settings-block settings-actions" role="group" aria-label="Storage cleanup">
          {something ? <button type="button" className="danger-button" data-storage-prune onClick={() => setPhase("asking")}><Icon name="trash" size={16} />Clean up</button> : null}
          <button type="button" data-cancel onClick={() => { setPreview(null); setPhase("idle"); }}>Close</button>
        </div>
      ) : null}
      {phase === "asking" ? (
        <div className="settings-block">
          <Confirm question={question} answer="Yes, clean up" tone="danger" onConfirm={send} onCancel={() => setPhase("previewed")} />
        </div>
      ) : null}
      {lines.length > 0 ? <div className="settings-block"><ul role="status" className="storage-result">{lines.map((line, index) => <li key={index}>{line}</li>)}</ul></div> : null}
      <SettingsNotes sending={phase === "sending"} sendingText="Cleaning up…" note={note} conflict={false} />
    </>
  );
}

function SettingsView({ settings, token, onSignedOut, onChanged }) {
  const on = settings.remote_actions === true;
  return (
    <>
      <section className="card settings-card">
        <SettingsHead icon="sliders">Run settings</SettingsHead>
        <RemoteActionsSwitch key={String(on) + " " + settings.version} on={on} version={settings.version} token={token} onSignedOut={onSignedOut} onChanged={onChanged} />
      </section>
      <div className="settings-group-head" data-machine-defaults>
        <h2>Machine defaults</h2>
        <p>Each project can override these on its own Settings page.</p>
      </div>
      {[["Implementer", "implementer"], ["Reviewer", "reviewer"]].map(([title, role]) => (
        <section key={role} className="card settings-card">
          {on ? (
            <AgentEditor key={settings.version} title={title} role={role} agent={settings[role]} version={settings.version} token={token} onSignedOut={onSignedOut} onChanged={onChanged} />
          ) : (
            <>
              <SettingsHead icon="cpu">{title}</SettingsHead>
              <AgentFields agent={settings[role]} />
            </>
          )}
        </section>
      ))}
      {on ? (
        <section className="card settings-card">
          <SettingsHead icon="folder">Storage</SettingsHead>
          <StorageCleanup token={token} onSignedOut={onSignedOut} />
        </section>
      ) : null}
    </>
  );
}

const FEATURE_GROUPS = ["draft", "pending", "in_progress", "completed", "abandoned"];

// The work still to do comes first: in progress, then pending, then drafts; then completed, abandoned, and any
// state the list does not know. Within a state the ledger's own order holds.
const FEATURE_PRIORITY = ["in_progress", "pending", "draft", "completed", "abandoned"];
const featureRank = (feature) => {
  const at = FEATURE_PRIORITY.indexOf(feature.status);
  return at === -1 ? FEATURE_PRIORITY.length : at;
};
const unfinishedFirst = (features) => [...features].sort((a, b) => featureRank(a) - featureRank(b));

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

// With a check, the button first reads that path and asks question(reply), which says the question or null
// when the reply does not answer it; a refused check shows its reason and asks nothing. done(run, reply)
// says what happened.
// The reply's JSON, or null when it has none.
async function jsonOrNull(response) {
  try {
    return await response.json();
  } catch (unreadable) {
    return null;
  }
}

// A button in the design system's tone: "primary" moves work forward, "danger" ends or throws it away.
function toneClass(tone) {
  return tone === "primary" ? "primary-button" : tone === "danger" ? "danger-button" : undefined;
}

// A question as its own block: any choices, the sentence, then the answer in the action's tone and Cancel beside it.
function Confirm({ question, detail = null, answer, tone, busy = false, onConfirm, onCancel, children }) {
  return (
    <div className="confirm" role="group" aria-label={question}>
      {children ? <div className="confirm-fields">{children}</div> : null}
      <p className="confirm-question">{question}{detail !== null ? <span className="confirm-detail muted">{detail}</span> : null}</p>
      <div className="confirm-actions">
        <button type="button" className={toneClass(tone === "danger" ? "danger" : "primary")} data-confirm disabled={busy} onClick={onConfirm}>{answer}</button>
        <button type="button" data-cancel disabled={busy} onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

// The link a start's confirmation carries to the new run's orchestration page, or nothing for an empty or
// unroutable run id.
function OpenRun({ projectId, run }) {
  if (!RUN_ID.test(run)) return null;
  const href = "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(run);
  return <a data-open-run={run} href={href} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>Open run</a>;
}

// opensIn: the project whose orchestration page the run the action started is linked to, or null for an
// action that starts no run.
function RunAction({ kind, label, question, path, check, token, onSignedOut, onDone, done, tone, icon, asking = false, onCancel = null, opensIn = null }) {
  const [phase, setPhase] = useState(asking ? "asking" : "idle");
  const [note, setNote] = useState("");
  const [run, setRun] = useState("");
  const [asked, setAsked] = useState(typeof question === "string" ? question : "");
  const ask = async () => {
    setNote("");
    if (check === undefined) {
      setPhase("asking");
      return;
    }
    setPhase("checking");
    let text = "The run could not be checked.";
    try {
      const response = await get(check, token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      const posed = response.ok && body !== null ? question(body) : null;
      if (typeof posed === "string") {
        setAsked(posed);
        setPhase("asking");
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (!response.ok && body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const send = async () => {
    setPhase("sending");
    let text = "The action could not be completed.";
    try {
      const response = await actionRequest(path, token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        // Done for good: the control stays closed while the list is read again, so the same action cannot be sent twice.
        const started = body !== null && typeof body.run === "string" ? body.run : "";
        setNote(done(started, body ?? {}));
        setRun(started);
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
    <div className="run-action">
      {phase === "idle" ? <button type="button" className={toneClass(tone)} {...{ ["data-" + kind]: "" }} onClick={ask}>{icon ? <Icon name={icon} size={16} /> : null}{label}</button> : null}
      {phase === "checking" ? <span role="status">Checking…</span> : null}
      {phase === "asking" ? <Confirm question={asked} answer={"Yes, " + label.toLowerCase()} tone={tone} onConfirm={send} onCancel={() => (onCancel === null ? setPhase("idle") : onCancel())} /> : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
      {phase === "done" && opensIn !== null ? <OpenRun projectId={opensIn} run={run} /> : null}
    </div>
  );
}

const MODEL_ROLES = ["implementer", "reviewer"];

// Reads the profiles the launch picker offers: {reply} with at least one profile, or with none when
// `startsRuns` and the server offers Project default, {signedOut} on a refused sign-in, or the reason to
// show: none defined, not allowed, the server's error, or unreachable.
async function readLaunchProfiles(control, startsRuns = false) {
  let text = "The model profiles could not be read.";
  try {
    const response = await get(controlPath(control, "launch-profiles"), control.token);
    if (response.status === 401) return { signedOut: true, reply: null, text: "" };
    const body = await jsonOrNull(response);
    const names = response.ok && body !== null && Array.isArray(body.profiles) ? body.profiles.filter((name) => typeof name === "string") : null;
    const choice = names !== null && (names.length > 0 || (startsRuns && body.project_default === true));
    if (choice) return { signedOut: false, reply: { ...body, profiles: names }, text: "" };
    if (names !== null) text = "No model profiles are defined.";
    else if (response.status === 403) text = "Not allowed.";
    else if (body !== null && typeof body.error === "string") text = body.error;
  } catch (unreachable) {
    text = "The server could not be reached.";
  }
  return { signedOut: false, reply: null, text };
}

// The role's default when the server names one it offers, else the first profile.
function preselect(reply, role) {
  return typeof reply[role] === "string" && reply.profiles.includes(reply[role]) ? reply[role] : reply.profiles[0];
}

// The popup's Switch model on a paused run: it reads the profiles the launch picker offers, preselects the
// role's default, and asks with the picked role and profile before posting them. A refused read or switch
// shows the server's reason; the switch takes effect when the run resumes.
function ModelAction({ run, control }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const [offer, setOffer] = useState(null);
  const [role, setRole] = useState("implementer");
  const [profile, setProfile] = useState("");
  const runPath = controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/model");
  const ask = async () => {
    setNote("");
    setPhase("checking");
    const { signedOut, reply, text } = await readLaunchProfiles(control);
    if (signedOut) {
      control.onSignedOut();
      return;
    }
    if (reply !== null) {
      setOffer(reply);
      setRole("implementer");
      setProfile(preselect(reply, "implementer"));
      setPhase("asking");
      return;
    }
    setNote(text);
    setPhase("idle");
  };
  const pickRole = (picked) => {
    setRole(picked);
    setProfile(preselect(offer, picked));
  };
  const send = async () => {
    setPhase("sending");
    let text = "The model could not be switched.";
    try {
      const response = await actionRequest(runPath, control.token, { role, profile });
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        setNote("The " + role + " of " + run.run_id + " continues on " + profile + " when it resumes.");
        setPhase("done");
        control.onDone();
        return;
      } else if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const asked = "Switch the " + role + " of " + run.run_id + " to " + profile + "? It takes effect when the run resumes.";
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" data-model="" onClick={ask}><Icon name="cpu" size={16} />Switch model</button> : null}
      {phase === "checking" ? <span role="status">Checking…</span> : null}
      {phase === "asking" ? (
        <Confirm question={asked} answer="Yes, switch model" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")}>
          <label>
            Role{" "}
            <select data-model-role="" value={role} onChange={(event) => pickRole(event.target.value)}>
              {MODEL_ROLES.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
          </label>
          <label>
            Profile{" "}
            <select data-model-profile="" value={profile} onChange={(event) => setProfile(event.target.value)}>
              {offer.profiles.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
          </label>
        </Confirm>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
    </div>
  );
}

// Project default: a start that names no profiles, so the run uses the project's own agents.
const PROJECT_DEFAULT = "";

// The launch picker's two choices, the implementer and the reviewer, each from the offered profiles, and
// Project default when the server offers it.
function LaunchPickers({ profiles, projectDefault, implementer, reviewer, onImplementer, onReviewer }) {
  const options = [
    ...(projectDefault ? [<option key={PROJECT_DEFAULT} value={PROJECT_DEFAULT}>Project default</option>] : []),
    ...profiles.map((name) => <option key={name} value={name}>{name}</option>),
  ];
  return (
    <>
      <label>
        Implementer{" "}
        <select data-launch-implementer="" value={implementer} onChange={(event) => onImplementer(event.target.value)}>{options}</select>
      </label>
      <label>
        Reviewer{" "}
        <select data-launch-reviewer="" value={reviewer} onChange={(event) => onReviewer(event.target.value)}>{options}</select>
      </label>
    </>
  );
}

// The pair a launch picker holds. begin(reply) preselects the server's defaults, or Project default for both
// roles when the server offers it. A start names both profiles or neither, so Project default goes for both
// roles: picking it for one picks it for the other, and picking a profile for one gives the other its
// preselected profile. body is what the start route is sent, and phrase how the question names the pair.
function useLaunchChoice() {
  const [offer, setOffer] = useState(null);
  const [implementer, setImplementer] = useState("");
  const [reviewer, setReviewer] = useState("");
  const begin = (reply) => {
    const onDefault = reply.project_default === true;
    setOffer(reply);
    setImplementer(onDefault ? PROJECT_DEFAULT : preselect(reply, "implementer"));
    setReviewer(onDefault ? PROJECT_DEFAULT : preselect(reply, "reviewer"));
  };
  const pick = (set, other, setOther, otherRole) => (name) => {
    set(name);
    if (name === PROJECT_DEFAULT) setOther(PROJECT_DEFAULT);
    else if (other === PROJECT_DEFAULT) setOther(preselect(offer, otherRole));
  };
  const onDefault = implementer === PROJECT_DEFAULT || reviewer === PROJECT_DEFAULT;
  const pickers =
    offer === null ? null : (
      <LaunchPickers
        profiles={offer.profiles}
        projectDefault={offer.project_default === true}
        implementer={implementer}
        reviewer={reviewer}
        onImplementer={pick(setImplementer, reviewer, setReviewer, "reviewer")}
        onReviewer={pick(setReviewer, implementer, setImplementer, "implementer")}
      />
    );
  return {
    begin,
    pickers,
    body: onDefault ? {} : { implementer, reviewer },
    phrase: onDefault ? "the project's default agents" : implementer + " as implementer and " + reviewer + " as reviewer",
  };
}

// The popup's launch picker, "Start with…" on a pending feature: it reads the launch profiles, preselects the
// implementer and reviewer defaults, and asks with the picked pair before posting them to the start route.
// A refused read or start shows the server's reason.
function LaunchAction({ feature, control }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const [run, setRun] = useState("");
  const choice = useLaunchChoice();
  const startPath = controlPath(control, "features/" + encodeURIComponent(feature.name) + "/runs");
  const ask = async () => {
    setNote("");
    setPhase("checking");
    const { signedOut, reply, text } = await readLaunchProfiles(control, true);
    if (signedOut) {
      control.onSignedOut();
      return;
    }
    if (reply !== null) {
      choice.begin(reply);
      setPhase("asking");
      return;
    }
    setNote(text);
    setPhase("idle");
  };
  const send = async () => {
    setPhase("sending");
    let text = "The run could not be started.";
    try {
      const response = await actionRequest(startPath, control.token, choice.body);
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        const run = body !== null && typeof body.run === "string" ? body.run : "";
        setNote(run === "" ? "Started." : "Started run " + run + ".");
        setRun(run);
        setPhase("done");
        control.onDone();
        return;
      } else if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const asked = "Start " + feature.name + " with " + choice.phrase + "?";
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="primary-button" data-start-with="" onClick={ask}><Icon name="sliders" size={16} />Start with…</button> : null}
      {phase === "checking" ? <span role="status">Checking…</span> : null}
      {phase === "asking" ? (
        <Confirm question={asked} answer="Yes, start" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")}>
          {choice.pickers}
        </Confirm>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
      {phase === "done" ? <OpenRun projectId={control.projectId} run={run} /> : null}
    </div>
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
      {section === "settings" ? null : <SettingsLink projectId={projectId} />}
    </div>
  );
}

// The cog beside the picker: the chosen project's Settings page, or this machine's while All projects is chosen.
function SettingsLink({ projectId }) {
  const href = projectId === null ? "/settings" : "/projects/" + encodeURIComponent(projectId) + "/settings";
  return (
    <a className="project-settings-link" data-settings-link href={href} aria-label={projectId === null ? "Settings" : "Project settings"} title={projectId === null ? "Settings" : "Project settings"} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>
      <Icon name="gear" size={18} />
    </a>
  );
}


function FeatureState({ status }) {
  const icon = { draft: "doc", pending: "clock", in_progress: "play", completed: "check", abandoned: "close" }[status] ?? "flag";
  return <span className={"feature-state state-" + (FEATURE_GROUPS.includes(status) ? status : "other")}><Icon name={icon} size={14} />{status}</span>;
}

// Each feature is a card that opens its own page. Search, state, stage and sort narrow the list;
// the newest activity comes first.
const FEATURES_PER_PAGE = 10;

// The one pager every paged list uses: where it is, then Previous and Next side by side with their chevrons.
// prev and next are {attrs, onClick} or null when there is no page that way.
function PagerBar({ status, prev, next, label = "Pagination" }) {
  return (
    <nav className="pager" aria-label={label}>
      <span className="pager-status">{status}</span>
      <span className="pager-buttons">
        <button type="button" {...prev.attrs} disabled={prev.onClick === null} onClick={prev.onClick ?? undefined}><Icon name="chevronLeft" size={16} />Previous</button>
        <button type="button" {...next.attrs} disabled={next.onClick === null} onClick={next.onClick ?? undefined}>Next<Icon name="chevronRight" size={16} /></button>
      </span>
    </nav>
  );
}

// Previous and Next under a list longer than one page.
function Pager({ at, pages, onPage }) {
  if (pages <= 1) return null;
  return <PagerBar status={"Page " + (at + 1) + " of " + pages} prev={{ attrs: { "data-page-prev": "" }, onClick: at === 0 ? null : () => onPage(at - 1) }} next={{ attrs: { "data-page-next": "" }, onClick: at === pages - 1 ? null : () => onPage(at + 1) }} />;
}

// "in alpha" for the every-project list's buttons and questions, so each names the project it sends to.
const placeSuffix = (place) => (place === null ? "" : " in " + place);
const featuresPhrase = (names, place) => names.length + (names.length === 1 ? " feature" : " features") + placeSuffix(place) + ": " + names.join(", ");

// Promote marked: it asks once, then sends one promote request per marked draft in ledger order, so the
// server audits each on its own; there is no bulk route. A refused sign-in stops the rest.
function PromoteMarked({ names, control, onFinished, place = null }) {
  const [phase, setPhase] = useState("idle");
  const label = "Promote " + names.length + placeSuffix(place);
  const send = async () => {
    setPhase("sending");
    const results = [];
    for (const name of names) {
      let text = name + ": it could not be promoted.";
      let ok = false;
      try {
        const response = await actionRequest(controlPath(control, "features/" + encodeURIComponent(name) + "/promote"), control.token);
        if (response.status === 401) {
          control.onSignedOut();
          return;
        }
        const body = await jsonOrNull(response);
        ok = response.ok;
        if (ok) text = "Promoted " + name + "; it is pending.";
        else if (response.status === 403) text = name + ": Not allowed.";
        else if (body !== null && typeof body.error === "string") text = name + ": " + body.error;
      } catch (unreachable) {
        text = name + ": The server could not be reached.";
      }
      results.push({ name, ok, text });
    }
    setPhase("idle");
    onFinished(results);
  };
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="primary-button" data-promote-marked onClick={() => setPhase("asking")}><Icon name="arrowUp" size={16} />{label}</button> : null}
      {phase === "asking" ? (
        <Confirm question={"Promote " + featuresPhrase(names, place) + "?"} answer="Yes, promote" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")} />
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
    </div>
  );
}

// The marked features in dependency order: each comes after the marked features it depends on, and
// otherwise in ledger order.
function dependencyOrder(features, names) {
  const marked = features.filter((feature) => names.includes(feature.name));
  const placed = [];
  const place = (feature, seen) => {
    if (placed.includes(feature.name) || seen.includes(feature.name)) return;
    for (const dep of Array.isArray(feature.depends_on) ? feature.depends_on : []) {
      const before = marked.find((entry) => entry.name === dep);
      if (before) place(before, [...seen, feature.name]);
    }
    placed.push(feature.name);
  };
  marked.forEach((feature) => place(feature, []));
  return placed;
}

// The popup's launch marked: "Start N" reads the launch profiles, asks with the picked pair, then sends
// one start per marked feature in the order given, and reports each run started or refused.
function StartMarked({ names, control, onFinished, place = null }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const choice = useLaunchChoice();
  const label = "Start " + names.length + placeSuffix(place);
  const ask = async () => {
    setNote("");
    setPhase("checking");
    const { signedOut, reply, text } = await readLaunchProfiles(control, true);
    if (signedOut) {
      control.onSignedOut();
      return;
    }
    if (reply !== null) {
      choice.begin(reply);
      setPhase("asking");
      return;
    }
    setNote(text);
    setPhase("idle");
  };
  const send = async () => {
    setPhase("sending");
    const results = [];
    for (const name of names) {
      let text = name + ": the run could not be started.";
      let ok = false;
      let started = "";
      try {
        const response = await actionRequest(controlPath(control, "features/" + encodeURIComponent(name) + "/runs"), control.token, choice.body);
        if (response.status === 401) {
          control.onSignedOut();
          return;
        }
        const body = await jsonOrNull(response);
        ok = response.ok;
        const run = body !== null && typeof body.run === "string" ? body.run : "";
        if (ok) {
          text = run === "" ? "Started " + name + "." : "Started run " + run + " for " + name + ".";
          started = run;
        }
        else if (response.status === 403) text = name + ": Not allowed.";
        else if (body !== null && typeof body.error === "string") text = name + ": " + body.error;
      } catch (unreachable) {
        text = name + ": The server could not be reached.";
      }
      results.push({ name, ok, text, run: started, projectId: control.projectId });
    }
    setPhase("idle");
    onFinished(results);
  };
  const asked = "Start " + featuresPhrase(names, place) + " with " + choice.phrase + "?";
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="primary-button" data-start-marked onClick={ask}><Icon name="play" size={16} />{label}</button> : null}
      {phase === "checking" ? <span role="status">Checking…</span> : null}
      {phase === "asking" ? (
        <Confirm question={asked} answer="Yes, start" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")}>
          {choice.pickers}
        </Confirm>
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
    </div>
  );
}

// The editable fields a reply of …/features/{name}/fields carries, as the editor holds them: text for the
// boxes, the parallelizable choice as "", "yes" or "no", and the dependencies as a list of names.
function featureDraft(fields) {
  const text = (value) => (typeof value === "string" ? value : "");
  const list = (value) => (isTextList(value) ? value : []);
  return {
    name: text(fields.name),
    description: text(fields.description),
    test_file: text(fields.test_file),
    target_files: list(fields.target_files).join(", "),
    parallelizable: fields.parallelizable === true ? "yes" : fields.parallelizable === false ? "no" : "",
    depends_on: list(fields.depends_on),
  };
}

// What the editor sends: a blank box is null, the target files one path per comma or line, and the
// dependencies in ledger order.
function featureFields(draft, features, empty = null) {
  const blank = (value) => (value.trim() === "" ? empty : value);
  return {
    name: draft.name.trim(),
    description: blank(draft.description),
    depends_on: features.map((feature) => feature.name).filter((name) => draft.depends_on.includes(name)),
    parallelizable: draft.parallelizable === "yes" ? true : draft.parallelizable === "no" ? false : null,
    test_file: blank(draft.test_file),
    target_files: draft.target_files.split(/[,\n]/).map((path) => path.trim()).filter((path) => path !== ""),
  };
}

// What an edit sends: only the fields changed since the editor opened, so a field left alone (and any
// redacted text it was shown) is not sent back, and an emptied box is sent as a clear ("" or []).
function changedFields(draft, opened, features) {
  const now = featureFields(draft, features, "");
  const was = featureFields(opened, features, "");
  return Object.fromEntries(Object.keys(now).filter((key) => key !== "name" && JSON.stringify(now[key]) !== JSON.stringify(was[key])).map((key) => [key, now[key]]));
}

// Edit (name given) or Add feature (no name): the popup's editor as plain inputs. Edit reads the fields
// and the ledger's version first, so the save goes ahead only if nobody changed the ledger meanwhile.
// Save asks first; refused values keep the editor open with the reason, and a changed ledger offers Reload.
// How many features "Depends on" offers beyond the chosen ones; a larger ledger is searched.
const DEPENDS_SHOWN = 8;

// "Depends on": the chosen features first, then at most DEPENDS_SHOWN more that match the search.
function DependsPicker({ others, chosen, onToggle }) {
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  const picked = others.filter((feature) => chosen.includes(feature.name));
  const matching = others.filter((feature) => !chosen.includes(feature.name) && feature.name.toLowerCase().includes(needle));
  const offered = matching.slice(0, DEPENDS_SHOWN);
  const searchable = others.length > DEPENDS_SHOWN;
  return (
    <fieldset className="feature-editor-depends">
      <legend className="field-label">Depends on</legend>
      {searchable ? (
        <label className="search-box">
          <Icon name="search" size={18} />
          <input type="search" data-depends-search aria-label="Search the features to depend on" placeholder="Search features" autoComplete="off" spellCheck={false} value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
      ) : null}
      <div className="depends-options">
        {[...picked, ...offered].map((feature) => (
          <label key={feature.name} className="depends-option"><input type="checkbox" data-field-depends={feature.name} checked={chosen.includes(feature.name)} onChange={() => onToggle(feature.name)} />{feature.name}</label>
        ))}
      </div>
      {matching.length > offered.length ? <p className="field-hint muted">{"Showing " + offered.length + " of " + matching.length + " matches; search to narrow them."}</p> : null}
      {searchable && matching.length === 0 ? <p className="field-hint muted">No other features match.</p> : null}
    </fieldset>
  );
}

function FeatureEditor({ name = null, features, control }) {
  const adding = name === null;
  const [phase, setPhase] = useState("idle");
  const [draft, setDraft] = useState(null);
  const [opened, setOpened] = useState(null);
  const [version, setVersion] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  // Adding opens in a popup (on a phone the stylesheet lays it in the page); Close or Escape shuts it unsent.
  const popup = adding && (phase === "editing" || phase === "asking" || phase === "sending");
  const close = () => {
    setNote("");
    setConflict(false);
    setPhase("idle");
  };
  useEffect(() => {
    if (!popup || phase === "sending") return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") close();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [popup, phase]);
  const open = async () => {
    setNote("");
    setConflict(false);
    if (adding) {
      setDraft(featureDraft({}));
      setVersion(null);
      setPhase("editing");
      return;
    }
    setPhase("loading");
    let text = "The feature could not be read.";
    try {
      const response = await get(controlPath(control, "features/" + encodeURIComponent(name) + "/fields"), control.token);
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok && body !== null && body.fields !== null && typeof body.fields === "object" && typeof body.version === "string") {
        setDraft(featureDraft(body.fields));
        setOpened(featureDraft(body.fields));
        setVersion(body.version);
        setPhase("editing");
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const fields = draft === null ? null : featureFields(draft, features);
  const shownName = adding ? (fields === null ? "" : fields.name) : name;
  const send = async () => {
    setPhase("sending");
    let text = "The feature could not be saved.";
    let changed = false;
    try {
      const path = controlPath(control, adding ? "features" : "features/" + encodeURIComponent(name));
      const response = await actionRequest(path, control.token, adding ? { fields } : { fields: changedFields(draft, opened, features), version });
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        // Done for good: the editor stays closed while the ledger is read again.
        setNote((adding ? "Added " : "Saved ") + shownName + ".");
        setPhase("done");
        control.onDone();
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
      changed = response.status === 409;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setConflict(changed);
    setPhase("editing");
  };
  const set = (key) => (event) => setDraft({ ...draft, [key]: event.target.value });
  const toggle = (dependency) => setDraft({ ...draft, depends_on: draft.depends_on.includes(dependency) ? draft.depends_on.filter((entry) => entry !== dependency) : [...draft.depends_on, dependency] });
  const question = (adding ? "Add " : "Save ") + shownName + "?";
  const others = features.filter((feature) => feature.name !== name);
  const parts = (
    <>
      {phase === "editing" ? (
        <div role="group" aria-label={adding ? "Add a feature" : "Edit " + name} className="feature-editor-fields">
          {adding ? <label className="field"><span className="field-label">Name</span><input type="text" data-field-name value={draft.name} autoComplete="off" spellCheck={false} onChange={set("name")} /></label> : null}
          <label className="field field-wide"><span className="field-label">Description</span><input type="text" data-field-description value={draft.description} onChange={set("description")} /></label>
          <label className="field"><span className="field-label">Test file</span><input type="text" data-field-test-file value={draft.test_file} autoComplete="off" spellCheck={false} onChange={set("test_file")} /></label>
          <label className="field"><span className="field-label">Target files</span><input type="text" data-field-target-files value={draft.target_files} autoComplete="off" spellCheck={false} placeholder="src/a.rs, src/b.rs" onChange={set("target_files")} /></label>
          <label className="field"><span className="field-label">Parallelizable</span><select data-field-parallelizable value={draft.parallelizable} onChange={set("parallelizable")}><option value="">Not set</option><option value="yes">Yes</option><option value="no">No</option></select></label>
          {others.length > 0 ? <DependsPicker others={others} chosen={draft.depends_on} onToggle={toggle} /> : null}
          <div className="feature-editor-actions">
            <button type="button" className="primary-button" data-feature-save onClick={() => { setNote(""); setConflict(false); setPhase("asking"); }}><Icon name="check" size={16} />{adding ? "Add" : "Save"}</button>
            <button type="button" data-cancel onClick={() => { setNote(""); setConflict(false); setPhase("idle"); }}>Cancel</button>
          </div>
        </div>
      ) : null}
      {phase === "asking" ? (
        <Confirm question={question} answer={adding ? "Yes, add" : "Yes, save"} tone="primary" onConfirm={send} onCancel={() => setPhase("editing")} />
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
      {conflict ? <button type="button" data-feature-reload onClick={() => { setNote(""); setConflict(false); setPhase("idle"); control.onDone(); }}>Reload</button> : null}
    </>
  );
  return (
    <div className="feature-editor">
      {phase === "idle" ? (adding ? <button type="button" className="primary-button" data-feature-add onClick={open}><Icon name="plus" size={16} />Add feature</button> : <button type="button" data-feature-edit onClick={open}><Icon name="pencil" size={16} />Edit</button>) : null}
      {phase === "loading" ? <span role="status">Loading the feature…</span> : null}
      {popup ? (
        <div className="feature-modal" data-feature-modal>
          <div className="feature-modal-box" role="dialog" aria-modal="true" aria-labelledby="feature-modal-title">
            <div className="feature-modal-head">
              <h2 id="feature-modal-title">Add feature</h2>
              <button type="button" className="icon-button" data-modal-close aria-label="Close" disabled={phase === "sending"} onClick={close}><Icon name="close" size={18} /></button>
            </div>
            {parts}
          </div>
        </div>
      ) : parts}
    </div>
  );
}

// A sideways swipe across a row: left (onSwipe(true)) or right (onSwipe(false)) when it moved more across than
// down, so scrolling the list is not mistaken for one.
function swipeHandlers(touchAt, onSwipe) {
  return {
    onTouchStart: (event) => {
      const point = event.touches[0];
      touchAt.current = point ? { x: point.clientX, y: point.clientY } : null;
    },
    onTouchEnd: (event) => {
      const from = touchAt.current;
      const point = event.changedTouches[0];
      touchAt.current = null;
      if (!from || !point) return;
      const dx = point.clientX - from.x;
      if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(point.clientY - from.y)) onSwipe(dx < 0);
    },
  };
}

// The action a swipe reveals on a phone, beside the row's state: Promote for a draft, Start for a pending feature.
function SwipeButton({ feature, onPress }) {
  const draft = feature.status === "draft";
  const kind = draft ? "data-swipe-promote" : "data-swipe-start";
  return (
    <button type="button" className="primary-button" {...{ [kind]: feature.name }} onClick={onPress}>
      <Icon name={draft ? "arrowUp" : "play"} size={16} />{draft ? "Promote" : "Start"}
    </button>
  );
}

// One feature's Start (pending) or Promote (draft), on its page or under a swiped row; a swiped row's opens
// on its question, and cancelling it closes the row's action. Keyed by the feature's status where it is shown,
// so a draft just promoted offers a fresh Start rather than the finished Promote.
function FeatureAction({ feature, control, asking = false, onCancel = null }) {
  const name = encodeURIComponent(feature.name);
  const shared = { token: control.token, onSignedOut: control.onSignedOut, onDone: control.onDone, tone: "primary", asking, onCancel };
  return feature.status === "pending"
    ? <RunAction {...shared} kind="start" label="Start" icon="play" opensIn={control.projectId} question={"Start " + feature.name + "?"} path={controlPath(control, "features/" + name + "/runs")} done={(run) => (run === "" ? "Started." : "Started run " + run + ".")} />
    : <RunAction {...shared} kind="promote" label="Promote" icon="arrowUp" question={"Promote " + feature.name + "?"} path={controlPath(control, "features/" + name + "/promote")} done={(_, reply) => "Promoted " + (typeof reply.feature === "string" ? reply.feature : feature.name) + "; it is pending."} />;
}

function FeatureList({ features, showProject = false, control = null }) {
  const [query, setQuery] = useState("");
  const [state, setState] = useState("all");
  const [page, setPage] = useState(0);
  const [marked, setMarked] = useState([]);
  const [results, setResults] = useState([]);
  const [swiped, setSwiped] = useState(null);
  const [asking, setAsking] = useState(null);
  const touchAt = useRef(null);
  const adder = control === null ? null : <FeatureEditor features={features} control={control} />;
  if (features.length === 0) {
    return (
      <>
        {adder}
        <p>No features in this project's ledger.</p>
      </>
    );
  }
  const states = [...FEATURE_GROUPS, ...[...new Set(features.map((feature) => feature.status))].filter((status) => !FEATURE_GROUPS.includes(status))];
  const needle = query.trim().toLowerCase();
  const shown = unfinishedFirst(features).filter((feature) => (state === "all" || feature.status === state) && feature.name.toLowerCase().includes(needle));
  const pages = Math.max(1, Math.ceil(shown.length / FEATURES_PER_PAGE));
  const at = Math.min(page, pages - 1);
  const visible = shown.slice(at * FEATURES_PER_PAGE, (at + 1) * FEATURES_PER_PAGE);
  // A draft can be marked to promote and a pending feature to start, when its project allows actions: a row of
  // the every-project list carries its own project's control. Promotes go out in ledger order and starts in
  // dependency order, whatever order the marks were made in, each project's to that project.
  const controlOf = (feature) => (feature.control === undefined ? control : feature.control);
  const keyOf = (feature) => feature.projectId + "/" + feature.name;
  const markable = (feature) => controlOf(feature) !== null && (feature.status === "draft" || feature.status === "pending");
  const groups = [...new Set(features.filter(markable).map((feature) => feature.projectId))].map((projectId) => {
    const own = features.filter((feature) => feature.projectId === projectId && markable(feature));
    const names = own.filter((feature) => marked.includes(keyOf(feature))).map((feature) => feature.name);
    return {
      projectId,
      control: controlOf(own[0]),
      place: showProject ? own[0].projectName : null,
      promote: own.filter((feature) => feature.status === "draft" && names.includes(feature.name)).map((feature) => feature.name),
      start: dependencyOrder(own.filter((feature) => feature.status === "pending"), names),
    };
  }).filter((group) => group.promote.length > 0 || group.start.length > 0);
  const toggle = (key) => setMarked((now) => (now.includes(key) ? now.filter((entry) => entry !== key) : [...now, key]));
  const finished = (group) => (done) => {
    setMarked([]);
    setResults(done);
    if (done.some((result) => result.ok)) group.control.onDone();
  };
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
        {adder}
      </div>
      {groups.length > 0 ? (
        <div className="marked-actions">
          {groups.map((group) => (
            <Fragment key={group.projectId}>
              {group.promote.length > 0 ? <PromoteMarked names={group.promote} control={group.control} place={group.place} onFinished={finished(group)} /> : null}
              {group.start.length > 0 ? <StartMarked names={group.start} control={group.control} place={group.place} onFinished={finished(group)} /> : null}
            </Fragment>
          ))}
        </div>
      ) : null}
      {results.length > 0 ? (
        <ul className="marked-results" role="status">
          {results.map((result) => (
            <li key={result.name} data-marked-result className={result.ok ? "run-action-note" : "run-action-note error-text"}><span>{result.text}</span>{result.ok && typeof result.run === "string" ? <OpenRun projectId={result.projectId} run={result.run} /> : null}</li>
          ))}
        </ul>
      ) : null}
      {shown.length === 0 ? <p>No features match the filters.</p> : null}
      {shown.length > 0 ? (
        <div className="feature-table-wrap">
          <table className="feature-table">
            <thead>
              <tr><th scope="col">Feature <span className="feature-count" data-feature-count>{shown.length}</span></th><th scope="col">State</th></tr>
            </thead>
            <tbody>
              {visible.map((feature) => (
                <Fragment key={keyOf(feature)}>
                <tr className={swiped === keyOf(feature) ? "feature swiped" : "feature"} data-feature-row={feature.name} {...(markable(feature) ? swipeHandlers(touchAt, (left) => setSwiped(left ? keyOf(feature) : null)) : {})}>
                  <td>
                    {markable(feature) ? <input type="checkbox" className="feature-mark" data-mark={feature.name} aria-label={"Mark " + feature.name} checked={marked.includes(keyOf(feature))} onChange={() => toggle(keyOf(feature))} /> : null}
                    <a className="feature-link" href={featureHref(feature.projectId, feature.name)} data-feature-open={feature.name} title={showProject ? feature.projectName : undefined} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(featureHref(feature.projectId, feature.name)); } }}>{feature.name}</a>
                  </td>
                  <td>
                    <span className="feature-state-cell">
                      <FeatureState status={feature.status} />
                      {swiped === keyOf(feature) && markable(feature) ? <SwipeButton feature={feature} onPress={() => { setSwiped(null); setAsking(keyOf(feature)); }} /> : null}
                    </span>
                  </td>
                </tr>
                {asking === keyOf(feature) ? (
                  <tr className="swipe-confirm">
                    <td colSpan={2}><FeatureAction feature={feature} control={controlOf(feature)} asking onCancel={() => setAsking(null)} /></td>
                  </tr>
                ) : null}
                </Fragment>
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
// for it, and the action to start one when it is pending or to promote it when it is a draft.
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
        {control !== null ? (
          <div className="feature-detail-actions">
            <FeatureEditor key={feature.name} name={feature.name} features={features} control={control} />
            {feature.status === "pending" || feature.status === "draft" ? <FeatureAction key={feature.status} feature={feature} control={control} /> : null}
            {feature.status === "pending" ? <LaunchAction feature={feature} control={control} /> : null}
          </div>
        ) : null}
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
                  <RunState state={run.state} ending={run.ending} />
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

// Each row carries its own project's control, so a project that allows actions offers them here too.
function AllFeatures({ token, projects, reloads, onSignedOut, onRefresh }) {
  const toRows = (parsed, project) => {
    const control = parsed.actions ? { token, projectId: project.id, onSignedOut, onDone: onRefresh } : null;
    return tagged(parsed.rows, project.id, project.name).map((feature) => ({ ...feature, control }));
  };
  const state = useEveryProject({ token, projects, reloads, onSignedOut, part: "features", parse: parseFeatures, toRows });
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
        if (name === null) return <FeatureList features={tagged(data.rows, projectId, projectId)} control={control(data)} />;
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

// How a completed run that was delivered without a review ended: pushed, or merged into its base.
const RUN_ENDINGS = { pushed: "Pushed", merged: "Merged locally" };

// How a run delivers its work when it finishes, as its project's settings name it.
const RUN_DELIVERIES = { review: "Open review", push: "Push only", merge_local: "Merge locally" };

// The delivery a run's page shows, or null when the run has none the page knows.
function deliveryOf(run) {
  return typeof run.delivery === "string" && Object.hasOwn(RUN_DELIVERIES, run.delivery) ? RUN_DELIVERIES[run.delivery] : null;
}

function RunState({ state, ending }) {
  const known = Object.hasOwn(RUN_STATES, state);
  const label = !known ? "Unknown" : state === "completed" && typeof ending === "string" && Object.hasOwn(RUN_ENDINGS, ending) ? RUN_ENDINGS[ending] : RUN_STATES[state];
  return <span className={"run-state feature-state run-" + (known ? state : "unknown")}><Icon name={known ? RUN_ICONS[state] : "flag"} size={14} />{label}</span>;
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
  const actions = runs.some((run) => controlOf(run) !== null && CONTROLLED.includes(run.state));
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
                  <td><RunState state={run.state} ending={run.ending} /></td>
                  <td>{run.stage ? <span className="run-stage">{run.stage}</span> : <span className="muted" aria-label="No stage">–</span>}</td>
                  <td><Moment at={run.updated_at} /></td>
                  {actions ? (
                    <td>
                      <RowActions run={run} control={controlOf(run)} menu />
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

// The run states that have an action.
const CONTROLLED = ["running", "pause_requested", "paused", "failed", "blocked", "needs_review", "merged"];
const RESUMABLE = ["paused", "failed", "blocked", "needs_review"];
const DISCARDABLE = [...RESUMABLE, "merged"];
const ACCEPTABLE = ["failed", "needs_review"];

// Pause and Stop on a running run, Stop on one asked to pause, Resume on a paused, failed or blocked one and
// Review again on one that needs review, when the server allows actions. Resume asks the server where the run
// would continue before asking. Discard on the runs Resume is on and Clear on a merged one first ask what the
// branch would lose. Accept on a failed or needs_review run asks first; its tests then run on the server, so the
// result shows as the run's state once they finish. Switch model on a paused run picks a role and a profile.
// The run's row and its detail header show the same ones. In a row (menu) the first stays in sight, Pause or
// Resume, and the rest sit behind a ⋯ button; on a phone the CSS shows them all in the row instead.
function RowActions({ run, control, menu = false }) {
  if (control === null || !RUN_ID.test(run.run_id)) return null;
  const pause = run.state === "running" ? <RunAction kind="pause" label="Pause" icon="pause" question={"Pause " + run.run_id + "?"} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/pause")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Pause requested."} /> : null;
  const resume = RESUMABLE.includes(run.state) ? <RunAction kind="resume" tone="primary" icon="rotate" label={run.state === "needs_review" ? "Review again" : "Resume"} question={(reply) => (typeof reply.next === "string" ? "Resume " + run.run_id + " from " + reply.next + "?" : null)} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/resume")} check={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/resume-check")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next) => (next === "" ? "Resumed." : "Resumed as run " + next + ".")} /> : null;
  const stop = run.state === "running" || run.state === "pause_requested" ? <RunAction kind="stop" label="Stop" tone="danger" icon="stop" question={"Stop " + run.run_id + "? Its process ends and it can be resumed later."} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/stop")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Stopped."} /> : null;
  const discard = DISCARDABLE.includes(run.state) ? <RunAction kind="discard" tone="danger" icon="trash" label={run.state === "merged" ? "Clear" : "Discard"} question={(reply) => discardQuestion(run, reply)} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/discard")} check={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/discard-warning")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(gone, reply) => discardDone(gone || run.run_id, reply)} /> : null;
  const accept = ACCEPTABLE.includes(run.state) ? <RunAction kind="accept" label="Accept" tone="primary" icon="check" question={"Accept " + run.run_id + "? The tests run in its worktree; if they pass it is marked review ready."} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/accept")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(accepted) => "Accepting " + (accepted || run.run_id) + ": its tests are running. It shows as review ready when they pass."} /> : null;
  const model = run.state === "paused" ? <ModelAction run={run} control={control} /> : null;
  if (pause === null && resume === null && stop === null && discard === null && accept === null && model === null) return null;
  const all = [pause, resume, model, stop, accept, discard].filter((action) => action !== null);
  if (!menu || all.length === 1) return <span className="run-actions">{all}</span>;
  return <span className="run-actions">{all[0]}<MoreMenu label={"More actions for " + run.run_id}>{all.slice(1)}</MoreMenu></span>;
}

// A ⋯ button that opens its actions below it; Escape or a press elsewhere closes it. The actions stay mounted
// while it is closed, so a question one of them asked is still there when it opens again.
function MoreMenu({ label, children }) {
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
  return (
    <span className={"run-more" + (open ? " open" : "")} ref={box}>
      <button type="button" className="run-more-toggle" data-run-more aria-label={label} title="More actions" aria-haspopup="menu" aria-expanded={open ? "true" : "false"} onClick={() => setOpen(!open)}><Icon name="dots" size={18} /></button>
      <span className="run-more-menu">{children}</span>
    </span>
  );
}

// The popup's question before Discard, with what the branch would lose when the server says, or before Clear.
function discardQuestion(run, reply) {
  if (typeof reply.warning !== "string") return null;
  if (run.state === "merged") return "Clear " + run.run_id + "? Its work is on the target; its features stay, its record and logs go.";
  if (reply.warning === "") return "Discard " + run.run_id + "? Logs, a clean worktree and its branch go; uncommitted work is kept.";
  return "Discard " + run.run_id + "? Logs and a clean worktree go, and " + reply.warning + "; uncommitted work is kept.";
}

// "Discarded {run}." or "Cleared {run}.", then the server's note on what went and what is pending again.
function discardDone(run, reply) {
  const note = typeof reply.note === "string" ? reply.note.trim() : "";
  return (reply.cleared === true ? "Cleared " : "Discarded ") + run + "." + (note === "" ? "" : " " + note);
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
function RunTimeline({ timeline, listPath, runPath, follow, logs, control = null, startTab = "timeline" }) {
  const { run, events, next_cursor: next } = timeline;
  const delivery = deliveryOf(run);
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
            <RunState state={run.state} ending={run.ending} />
          </div>
          <RowActions run={run} control={control} />
          {validTime(run.started_at) === null && delivery === null ? null : (
            <p className="feature-detail-sub run-detail-sub">
              {validTime(run.started_at) === null ? null : <span>Started <Moment at={run.started_at} /></span>}
              {delivery === null ? null : <span data-run-delivery>{"Delivery: " + delivery}</span>}
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
      {onPrev !== null || next !== null ? <PagerBar label="Log pages" status={"Page " + number} prev={{ attrs: { "data-log-prev": "" }, onClick: onPrev }} next={{ attrs: { "data-more": "lines" }, onClick: next === null ? null : () => onNext(next) }} /> : null}
      <div className="log-pager">
        <span className="log-count">{shown.length + " of " + lines.length + " lines"}</span>
        <button type="button" className="log-download" data-log-download disabled={shown.length === 0} onClick={() => saveLines(LOG_FILE.test(page.name) ? page.name : "log.log", shown)}><Icon name="download" size={16} />Download</button>
      </div>
      {truncated && next === null ? <p>The log was cut short: only its first part is shown.</p> : null}
    </>
  );
}

// A project's own settings as the server gives them: each role the project's choice (or null) with what a
// run uses instead (or null), the delivery and the revision a save names.
const DELIVERIES = [["review", "Open review"], ["push", "Push only"], ["merge_local", "Merge locally"]];

function isPicked(agent) {
  return agent !== null && typeof agent === "object" && typeof agent.harness === "string" && typeof agent.model === "string" && (agent.effort === null || agent.effort === undefined || typeof agent.effort === "string");
}

function parseProjectSettings(data) {
  const settings = data && data.settings;
  const role = (entry) => entry !== null && typeof entry === "object" && (entry.project === null || isPicked(entry.project)) && (entry.fallback === null || isPicked(entry.fallback));
  const ok = settings && typeof settings === "object" && role(settings.implementer) && role(settings.reviewer) && DELIVERIES.some(([value]) => value === settings.delivery) && Number.isInteger(settings.revision) && settings.revision >= 0;
  return ok ? settings : null;
}

// An agent as one line: "claude sonnet, effort medium".
function agentLine(agent) {
  return agent.harness + " " + agent.model + (typeof agent.effort === "string" ? ", effort " + agent.effort : "");
}

// What a role runs with: the project's own agent, or the machine's default it falls back to.
function roleLine(entry) {
  if (entry.project !== null) return agentLine(entry.project);
  return entry.fallback === null ? "Machine default" : "Machine default (" + agentLine(entry.fallback) + ")";
}

function deliveryLabel(delivery) {
  return DELIVERIES.find(([value]) => value === delivery)[1];
}

// What a role's editor starts on: the project's own agent as it is, even one this machine no longer offers,
// so opening the editor never changes it, or the machine default (null).
function startingRole(agent) {
  return agent === null ? null : { harness: agent.harness, model: agent.model, effort: typeof agent.effort === "string" ? agent.effort : "" };
}

// The entries a select offers: the listed ones, after the current value when it is not among them.
function withCurrent(listed, current) {
  return listed.includes(current) ? listed.map((value) => [value, value]) : [[current, current + " (not offered)"], ...listed.map((value) => [value, value])];
}

// What Save sends for a role: null leaves it to the machine default.
function sentRole(choice) {
  return choice === null ? null : { harness: choice.harness, model: choice.model, effort: choice.effort || null };
}

// A role while editing: the harness, whose first entry is the machine default, then the model and effort
// of the harness picked, and a reset back to the machine default.
function RoleFields({ title, role, entry, options, choice, disabled, onChoose }) {
  const current = choice === null ? null : options.find((option) => option.harness === choice.harness);
  const levels = effortsOf(current);
  const harnesses = options.map((option) => option.harness);
  const pickHarness = (harness) => {
    const option = options.find((each) => each.harness === harness);
    onChoose(option ? { harness, model: option.models[0], effort: keptEffort(option, choice && choice.effort) } : null);
  };
  const machine = entry.fallback === null ? "Machine default" : "Machine default (" + agentLine(entry.fallback) + ")";
  return (
    <div className="settings-block project-role-fields">
      <select data-role-harness={role} aria-label={title + " harness"} value={choice === null ? "" : choice.harness} disabled={disabled} onChange={(event) => pickHarness(event.target.value)}>
        <option value="">{machine}</option>
        {(choice === null ? harnesses.map((harness) => [harness, harness]) : withCurrent(harnesses, choice.harness)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select>
      {choice !== null ? <select data-role-model={role} aria-label={title + " model"} value={choice.model} disabled={disabled} onChange={(event) => onChoose({ ...choice, model: event.target.value })}>{withCurrent(current ? current.models : [], choice.model).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select> : null}
      {current && levels.length > 0 ? <select data-role-effort={role} aria-label={title + " effort"} value={choice.effort} disabled={disabled} onChange={(event) => onChoose({ ...choice, effort: event.target.value })}><option value="">Not set</option>{(choice.effort === "" ? levels.map((level) => [level, level]) : withCurrent(levels, choice.effort)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select> : null}
      <button type="button" data-role-reset={role} disabled={disabled || choice === null} onClick={() => onChoose(null)}>Use machine default</button>
    </div>
  );
}

const PROJECT_ROLES = [["Implementer", "implementer"], ["Reviewer", "reviewer"]];

// A project's settings: each role and the delivery, read only until Edit. Edit checks that remote actions
// are on and reads the machine's options; Save asks, then sends the whole record with the revision Edit
// read, and a refusal shows the server's reason, with Reload when the settings changed meanwhile.
function ProjectSettingsView({ settings, path, token, onSignedOut, onChanged }) {
  const [phase, setPhase] = useState("idle");
  const [options, setOptions] = useState([]);
  const [draft, setDraft] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  const edit = async () => {
    setPhase("loading");
    setNote("");
    setConflict(false);
    try {
      const machine = await get(API_SETTINGS, token);
      if (machine.status === 401) {
        onSignedOut();
        return;
      }
      const read = machine.ok ? await jsonOrNull(machine) : null;
      if (!(read && read.settings && read.settings.remote_actions === true)) {
        setNote(read && read.settings ? "Turn remote actions on in Settings to change this project's settings." : "The settings could not be read.");
        setPhase("idle");
        return;
      }
      const response = await get(API_SETTINGS + "/options", token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const listed = response.ok ? parseOptions(await jsonOrNull(response)) : null;
      if (listed === null || listed.length === 0) {
        setNote("There are no harnesses to choose from.");
        setPhase("idle");
        return;
      }
      setOptions(listed);
      setDraft({ implementer: startingRole(settings.implementer.project), reviewer: startingRole(settings.reviewer.project), delivery: settings.delivery, revision: settings.revision });
      setPhase("editing");
    } catch (unreachable) {
      setNote("The server could not be reached.");
      setPhase("idle");
    }
  };
  const send = async () => {
    setPhase("sending");
    setNote("");
    setConflict(false);
    let text = "The change could not be made.";
    try {
      const response = await actionRequest(path, token, { implementer: sentRole(draft.implementer), reviewer: sentRole(draft.reviewer), delivery: draft.delivery, revision: draft.revision });
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.ok) {
        setPhase("idle");
        onChanged();
        return;
      }
      const reply = await jsonOrNull(response);
      if (reply !== null && typeof reply.error === "string") text = reply.error;
      setConflict(response.status === 409);
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("editing");
  };
  const editing = phase === "editing" || phase === "asking" || phase === "sending";
  const action = phase === "idle" ? <button type="button" data-project-settings-edit onClick={edit}><Icon name="pencil" size={16} />Edit</button> : phase === "loading" ? <span role="status" className="muted">Loading the options…</span> : null;
  return (
    <div className="settings-page project-settings">
      {PROJECT_ROLES.map(([title, role], index) => (
        <section key={role} className="card settings-card" data-project-role={role}>
          <SettingsHead icon="cpu" action={index === 0 ? action : null}>{title}</SettingsHead>
          {editing ? (
            <RoleFields title={title} role={role} entry={settings[role]} options={options} choice={draft[role]} disabled={phase !== "editing"} onChoose={(choice) => setDraft({ ...draft, [role]: choice })} />
          ) : (
            <div className="settings-block"><span>{roleLine(settings[role])}</span></div>
          )}
        </section>
      ))}
      <section className="card settings-card">
        <SettingsHead icon="flag">When a run finishes</SettingsHead>
        <div className="settings-block">
          {editing ? (
            <select data-delivery aria-label="When a run finishes" value={draft.delivery} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, delivery: event.target.value })}>
              {DELIVERIES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
          ) : (
            <span data-project-delivery>{deliveryLabel(settings.delivery)}</span>
          )}
        </div>
      </section>
      {phase === "editing" ? (
        <div className="settings-block settings-actions">
          <button type="button" className="primary-button" data-project-settings-save onClick={() => setPhase("asking")}><Icon name="check" size={16} />Save</button>
          <button type="button" data-cancel onClick={() => { setPhase("idle"); setNote(""); setConflict(false); }}>Cancel</button>
        </div>
      ) : null}
      {phase === "asking" ? (
        <Confirm question="Save this project's settings?" detail="Runs already going keep their agents and delivery." answer="Yes, save" tone="primary" onConfirm={send} onCancel={() => setPhase("editing")} />
      ) : null}
      <SettingsNotes sending={phase === "sending"} note={note} conflict={conflict} onReload={() => { setNote(""); setConflict(false); setPhase("idle"); onChanged(); }} />
    </div>
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
  if (/^\/projects\/[^/]+\/settings$/.test(address)) {
    const settingsPath = "/api/projects/" + encodeURIComponent(projectId) + "/settings";
    return (
      <Loaded key={settingsPath} reloads={reloads} header={() => (
        <>
          <PageHeader title="Project settings" subtitle="How this project's runs work, over the machine defaults" onRefresh={onRefresh} />
          <ProjectSelect projects={projects} projectId={projectId} section="settings" />
        </>
      )} token={token} path={settingsPath} loading="Loading the project's settings…" parse={parseProjectSettings} draw={(settings) => <ProjectSettingsView settings={settings} path={settingsPath} token={token} onSignedOut={onSignedOut} onChanged={onRefresh} />} onSignedOut={onSignedOut} />
    );
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
        )} token={token} path={timelinePath} loading="Loading the run…" parse={parseTimeline} draw={(data) => <RunTimeline key={address} timeline={data} listPath={listPath} runPath={runPath} startTab={logs !== null ? "logs" : "timeline"} logs={logsAt} follow={{ token, onSignedOut, cursor, base: apiRun }} control={data.remote_actions === true ? { token, projectId, onSignedOut, onDone: onRefresh } : null} />} onSignedOut={onSignedOut} />
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
          <PasskeysCard onSignOut={onSignOut} />
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
              {token === SESSION && <SignOutEverywhere onSignOut={onSignOut} />}
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
            <AllFeatures token={token} projects={projects} reloads={reloads} onSignedOut={onSignedOut} onRefresh={onRefresh} />
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
