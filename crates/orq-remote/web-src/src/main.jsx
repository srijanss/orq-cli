// The review inbox page. The bearer token is typed in, held in one piece of state and sent as a
// header on each request. A copy is kept in this tab's sessionStorage only, so a refresh does not
// ask again and closing the tab forgets it; it goes nowhere else (no address, no other storage).
// Every string from the API is rendered as text.
import { Fragment, cloneElement, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { createRoot } from "react-dom/client";
import { Shell, usePageRefresh, usePhone } from "./shell.jsx";
import { Badge, PILLS, StatusPill } from "./badge.jsx";
import { Icon } from "./icons.jsx";
import { Markdown } from "./markdown.jsx";
import { ThemeChoice, ThemeToggle } from "./theme.jsx";
import { readPins, writePins } from "./pins.js";
import { UNREACHABLE, authStatus, confirmWithPasskey, endAllSessions, listPasskeys, passkeyBlocker, passkeysAvailable, registerPasskey, removePasskey, renamePasskey, signInWithPasskey, signOutOfSession } from "./passkey.js";
import { applyUpdate, startWorker } from "./worker.js";
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
const API_SERVER_RESTART = "/api/server/restart";
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
      <button type="button" className="primary-button"onClick={go}>
        <Icon name="passkey" size={20} />
        Sign in with a passkey
      </button>
      {failed && <p role="alert">Could not sign in with a passkey.</p>}
    </div>
  );
}

function TokenPrompt({ message, onToken, passkeys = false, known = false, onPasskey, blocker = null, unreachable = false, onRetry }) {
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
      {unreachable ? (
        <>
          <p role="alert">Can't reach orq. Check that this device is connected to Tailscale, then try again.</p>
          <button type="button" className="primary-button"onClick={onRetry}>
            Retry
          </button>
          <ThemeToggle />
        </>
      ) : (
      <>
      {usable && !asking && <PasskeyButton onSignedIn={onPasskey} enrolled={passkeys} onAskToken={() => setAsking(true)} />}
      {blocker !== null && <p role="status">{blocker}</p>}
      {(!usable || asking) && (
        <>
          <p>Sign in with token: paste the access token printed by orq serve.</p>
          <label>
            Access token
            <input ref={field} type="password" autoComplete="off" spellCheck={false} aria-label="Access token" onKeyDown={(event) => event.key === "Enter" && submit()} />
          </label>
          <button type="button" className="primary-button"onClick={submit}>
            Sign in
          </button>
        </>
      )}
      <p role="alert">{message}</p>
      <div className="alternatives" />
      <ThemeToggle />
      </>
      )}
    </div>
    </div>
  );
}

// A risk the reference knows gets its coloured pill. A placeholder such as "-" means the review gave
// no rating, which a muted pill says plainly; any other word is shown as it came, in the same shape.
// "resolved" means the review found risks and every one is fixed; its label names the worst of them.
const RISK_KINDS = ["risk-high", "risk-medium", "risk-low"];

function RiskBadge({ risk, fixed }) {
  if (risk === undefined || risk === null) return null;
  const word = String(risk).trim();
  const kind = "risk-" + word.toLowerCase();
  if (kind === "risk-resolved") {
    const worst = typeof fixed === "string" ? fixed.trim() : "";
    return <StatusPill kind={kind} title={worst === "" ? "Risks fixed" : "Risks fixed (highest was " + worst + ")"} data-risk={kind} />;
  }
  if (RISK_KINDS.includes(kind)) return <StatusPill kind={kind} data-risk={kind} />;
  const unrated = word === "" || /^(-+|none|n\/a|unknown)$/i.test(word);
  return unrated ? <StatusPill kind="risk-none" data-risk="risk-none" /> : <StatusPill label={word + " risk"} icon="alert" tone="neutral" data-risk="risk-other" />;
}

// The short pill reads "Current", "Stale" or "Unverified"; for the last two a screen reader hears the
// full marker instead.
function FreshnessPill({ freshness }) {
  if (freshness !== "current" && freshness !== "stale" && freshness !== "unverified") return null;
  const marker = freshness === "stale" ? STALE_MARKER : freshness === "unverified" ? UNVERIFIED_MARKER : null;
  return (
    <StatusPill kind={freshness} data-freshness-pill={freshness}>
      {marker === null ? undefined : (
        <>
          <span aria-hidden="true">{PILLS[freshness].label}</span>
          <span className="sr-only">{marker}</span>
        </>
      )}
    </StatusPill>
  );
}

// A review's status as a pill: the words for a status the page knows, else the status itself in
// sentence case, neutral.
function ReviewStatus({ status }) {
  return <StatusPill kind={String(status)} data-review-status={String(status)} />;
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
        <ReviewStatus status={run.status} />
        <RiskBadge risk={run.risk} fixed={run.fixed_risk} />
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

// Risk choices go from most to least severe, then "resolved"; anything else follows by name.
const RISK_ORDER = ["critical", "high", "medium", "low", "resolved"];
const riskRank = (value) => (RISK_ORDER.includes(value) ? RISK_ORDER.indexOf(value) : RISK_ORDER.length);
const byRisk = (a, b) => riskRank(a) - riskRank(b) || (a < b ? -1 : a > b ? 1 : 0);

// A review still waits for a person until it is merged, dismissed (closed) or marked reviewed
// (resolved); the list shows only those until Status: All is chosen.
const REVIEW_DONE = ["merged", "closed", "resolved"];
const reviewWaiting = (status) => !REVIEW_DONE.includes(String(status));
const WAITING = "waiting";

const REVIEW_FILTERS = [
  ["status", "Status", (run) => String(run.status)],
  ["risk", "Risk", (run) => (run.risk === undefined || run.risk === null ? "" : String(run.risk)), byRisk],
  ["freshness", "Freshness", rowFreshness],
];

// A filter option says what its pill says ("High risk", not "high"); a value the pills do not know is shown as itself.
const pillWord = (kind, value) => (Object.hasOwn(PILLS, kind) ? PILLS[kind].label : value);
const FILTER_WORDS = { status: (value) => pillWord(value, value), risk: (value) => pillWord("risk-" + value.trim().toLowerCase(), value), freshness: (value) => pillWord(value, value) };

// Today's reviews or all of them, newest (or oldest) first, ten to a page; the filters only hide
// rows, so a marker never leaves a row that is shown. A filter, range or sort change starts again
// on the first page.
const REVIEWS_PER_PAGE = 10;

function ReviewList({ runs, detailPrefix = DETAIL_PREFIX, selected = null, onRows = null, showProject = false }) {
  const [chosen, setChosen] = useState({ status: WAITING });
  const [range, setRange] = useState("all");
  const [order, setOrder] = useState("desc");
  const [page, setPage] = useState(0);
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
    .filter((run) => REVIEW_FILTERS.every(([key, , read]) => !chosen[key] || (key === "status" && chosen[key] === WAITING ? reviewWaiting(run.status) : read(run) === chosen[key])))
    .sort((a, b) => (order === "desc" ? reviewTime(b) - reviewTime(a) : reviewTime(a) - reviewTime(b)));
  const flip = order === "desc" ? "asc" : "desc";
  const pages = Math.max(1, Math.ceil(shown.length / REVIEWS_PER_PAGE));
  const at = Math.min(page, pages - 1);
  const visible = shown.slice(at * REVIEWS_PER_PAGE, (at + 1) * REVIEWS_PER_PAGE);
  return (
    <div className="review-list card">
      <div className="review-list-head">
        <div className="review-range" role="group" aria-label="Which reviews">
          {[["today", "Today (" + today.length + ")"], ["all", "All"]].map(([id, label]) => (
            <button key={id} type="button" aria-pressed={range === id ? "true" : "false"} data-review-range={id} onClick={() => { setRange(id); setPage(0); }}>
              {label}
            </button>
          ))}
        </div>
        <button type="button" className="sort-toggle" data-review-sort={order} aria-label={order === "desc" ? "Sort by review time: newest first" : "Sort by review time: oldest first"} title={flip === "asc" ? "Show the oldest first" : "Show the newest first"} onClick={() => { setOrder(flip); setPage(0); }}>
          <Icon name={order === "desc" ? "sortDesc" : "sortAsc"} size={18} />
          {order === "desc" ? "Sort: Newest" : "Sort: Oldest"}
        </button>
      </div>
      <div className="filters review-filters" role="group" aria-label="Filter reviews">
        {REVIEW_FILTERS.map(([key, label, read, order]) => (
          <label key={key}>
            <span className="sr-only">{label}</span>
            <select data-filter={key} value={chosen[key] || ""} onChange={(event) => { setChosen({ ...chosen, [key]: event.target.value }); setPage(0); }}>
              {key === "status" ? <option value={WAITING}>{label + ": Waiting"}</option> : null}
              <option value="">{label + ": All"}</option>
              {[...new Set(runs.map(read).filter((value) => value !== ""))].sort(order).map((value) => (
                <option key={value} value={value}>{FILTER_WORDS[key](value)}</option>
              ))}
            </select>
          </label>
        ))}
      </div>
      {shown.length === 0 ? (
        <p className="review-empty">{range === "today" && today.length === 0 ? "No reviews today." : "No reviews match these filters."}</p>
      ) : (
        <ul className="review-rows">
          {visible.map((run) => (
            <ReviewRow key={run.run_id || run.feature} run={run} detailPrefix={detailPrefix} selected={selected !== null && run.run_id === selected} showProject={showProject} />
          ))}
        </ul>
      )}
      <Pager at={at} pages={pages} onPage={setPage} />
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

// The structured risks, open ones first by severity, then the fixed ones; a stable sort keeps the
// report's own order within a severity. Anything but an array of objects is not cards.
function sortedRisks(items) {
  const open = (item) => (item.fixed === true ? 1 : 0);
  return items.map((item, index) => ({ item, index })).sort((a, b) => open(a.item) - open(b.item) || riskRank(String(a.item.severity).toLowerCase()) - riskRank(String(b.item.severity).toLowerCase()) || a.index - b.index).map((entry) => entry.item);
}

function hasRiskCards(report) {
  return Array.isArray(report.risk_items) && report.risk_items.length > 0 && report.risk_items.every((item) => item !== null && typeof item === "object");
}

function RiskCard({ item }) {
  const severity = String(item.severity).trim().toLowerCase();
  const fixed = item.fixed === true;
  const href = safeHttpsLink(item.link);
  const place = typeof item.file === "string" && item.file !== "" ? item.file + (Number.isInteger(item.line) ? ":" + item.line : "") : null;
  return (
    <li className={"risk-card" + (fixed ? " risk-card-fixed" : "")} data-risk-card>
      <div className="risk-card-head">
        <StatusPill label={severity === "" ? "Unknown" : severity[0].toUpperCase() + severity.slice(1)} icon="alert" tone={severity === "critical" ? "risk-high" : "risk-" + severity} data-risk-severity />
        <span className="risk-card-state" data-risk-state>{fixed ? "Fixed" : "Open"}</span>
        {typeof item.category === "string" && item.category !== "" ? <span className="risk-card-category" data-risk-category>{item.category}</span> : null}
        {place === null ? null : href !== null ? (
          <a className="risk-card-place" data-risk-place href={href} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer"><code>{place}</code></a>
        ) : (
          <code className="risk-card-place" data-risk-place>{place}</code>
        )}
      </div>
      <div className="risk-card-claim"><Markdown text={String(item.claim)} /></div>
      {typeof item.reasoning === "string" && item.reasoning !== "" ? <div className="risk-card-reasoning"><Markdown text={item.reasoning} /></div> : null}
    </li>
  );
}

// The report beside (or, on a phone, instead of) the list. The row it was opened from, when the
// list has it, gives the feature, status and risk the report itself does not carry.
// actions: where the report's actions are posted and how they are sent, or null for none.
function Detail({ report, row = null, listPath = "/reviews", actions = null }) {
  const freshness = freshnessOf(report);
  const merged = shortHash(report.merged_locally);
  // An unverified review keeps the link it was opened with; a stale one, or one merged here, does not.
  // A report from a server that names no freshness gets none.
  const linkable = freshness === "current" || (freshness === "unverified" && report.freshness === "unverified");
  const href = merged === "none" && linkable && report.link_allowed === true ? safeHttpsLink(report.link) : null;
  const commit = shortHash(report.reviewed_head_sha);
  const status = typeof report.status === "string" ? report.status : row !== null ? String(row.status) : "";
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
          {row !== null ? <RiskBadge risk={row.risk} fixed={row.fixed_risk} /> : null}
          {row !== null ? <ReviewStatus status={row.status} /> : null}
        </span>
      </div>
      <p className="review-report-sub">
        {commit === "none" ? null : <code title={commit}>{commit.slice(0, 7)}</code>}
        <Generated at={report.generated_at} />
      </p>
      <FreshnessBanner report={report} freshness={freshness} />
      {merged !== "none" ? <p className="review-merged" data-merged-locally={merged}>{"Merged locally at "}<code title={merged}>{merged.slice(0, 7)}</code></p> : null}
      {actions !== null && status !== "" ? <ReviewActions key={status} status={status} head={report.reported_head_sha} {...actions} /> : null}
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
          {key === "risks" && hasRiskCards(report) ? (
            <ul className="risk-cards">
              {sortedRisks(report.risk_items).map((item, index) => (
                <RiskCard key={index} item={item} />
              ))}
            </ul>
          ) : (
            <ul>
              {(report[key].length === 0 ? ["none recorded"] : report[key]).map((line, index) => (
                <li key={index}>{line}</li>
              ))}
            </ul>
          )}
        </details>
      ))}
      {href !== null && (
        <p className="review-provider">
          <span className="muted">Provider:</span>
          <a href={href} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">
            {providerName(href)}
            <Icon name="external" size={16} />
          </a>
        </p>
      )}
    </article>
  );
}

// What a person can decide about a review without a PR/MR: mark it reviewed or dismiss it while it
// waits, reopen it once resolved or dismissed; a merged review is settled. Each asks first, and names
// the head the person read, so a newer report is never decided unseen.
const REVIEW_ACTIONS = {
  resolve: ["Mark reviewed", "Mark this review as reviewed?", "primary"],
  dismiss: ["Dismiss", "Dismiss this review? It stops waiting without a merge.", "danger"],
  reopen: ["Reopen", "Reopen this review? It waits for a person again.", "primary"],
};

function ReviewActions({ status, head, path, token, onSignedOut, onDone }) {
  const [asking, setAsking] = useState(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const offered = reviewWaiting(status) ? ["resolve", "dismiss"] : status === "resolved" || status === "closed" ? ["reopen"] : [];
  if (offered.length === 0) return null;
  const send = async (action) => {
    setBusy(true);
    let text = "The action could not be completed.";
    try {
      const response = await actionRequest(path + "/" + action, token, { head: typeof head === "string" ? head : null });
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        setAsking(null);
        setBusy(false);
        setNote("");
        onDone();
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setAsking(null);
    setBusy(false);
    setNote(text);
  };
  return (
    <div className="review-actions run-action">
      {asking === null
        ? offered.map((action) => (
            <button key={action} type="button" className={toneClass(REVIEW_ACTIONS[action][2])} data-review-action={action} onClick={() => { setNote(""); setAsking(action); }}>
              {REVIEW_ACTIONS[action][0]}
            </button>
          ))
        : <Confirm question={REVIEW_ACTIONS[asking][1]} answer={"Yes, " + REVIEW_ACTIONS[asking][0].toLowerCase()} tone={REVIEW_ACTIONS[asking][2]} busy={busy} onConfirm={() => send(asking)} onCancel={() => setAsking(null)} />}
      {busy ? <span role="status">Sending…</span> : null}
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
    </div>
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
            <Loaded key={path} reloads={reloads} back={listPath} token={token} path={path} loading="Loading the report…" parse={parseDetail} draw={(report) => <Detail report={report} row={row} listPath={listPath} actions={{ path, token, onSignedOut, onDone: onRefresh }} />} onSignedOut={onSignedOut} />
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
  const limit = project && project.max_active_runs;
  const blocked = project && project.start_blocked;
  return project !== null && typeof project === "object" && typeof project.id === "string" && RUN_ID.test(project.id) && typeof project.name === "string" && typeof project.available === "boolean" && ["features", "active_runs", "reviews_waiting", "failed_runs"].every((key) => isCount(project[key]))
    && (limit === undefined || limit === null || (Number.isInteger(limit) && limit > 0)) && (blocked === undefined || blocked === null || typeof blocked === "string");
}

// Why the project's settings refuse a run start now (its run limit or disk floor), or null when a start may go.
function startBlockedOf(project) {
  return project && typeof project.start_blocked === "string" && project.start_blocked !== "" ? project.start_blocked : null;
}

function parseProjects(data) {
  return data && Array.isArray(data.projects) && data.projects.every(isProject) ? data.projects : null;
}

// This machine's run limits as the listing gives them (its active runs, [runs] limits and free disk),
// or null when no machine limit is set or the answer is not one.
function parseMachine(machine) {
  const limit = (value) => value === null || (Number.isInteger(value) && value > 0);
  const ok = machine !== null && typeof machine === "object" && Number.isInteger(machine.active_runs) && machine.active_runs >= 0
    && limit(machine.max_active_runs) && limit(machine.min_free_disk_gb) && (machine.free_disk_gb === null || (typeof machine.free_disk_gb === "number" && machine.free_disk_gb >= 0));
  return ok ? machine : null;
}

function parseDashboard(data) {
  const projects = parseProjects(data);
  return projects === null ? null : { projects, machine: parseMachine(data.machine ?? null) };
}

// The dashboard's read-only line for the machine's run limits: how many runs are active of its
// limit, and its free disk against its floor.
function MachineRuns({ machine }) {
  const runs = machine.max_active_runs === null ? "Runs: " + machine.active_runs + " on this machine" : "Runs: " + machine.active_runs + " of " + machine.max_active_runs + " on this machine";
  const disk = machine.free_disk_gb === null ? null : machine.free_disk_gb + " GB free" + (machine.min_free_disk_gb === null ? "" : ", " + machine.min_free_disk_gb + " GB needed to start");
  return (
    <p className="machine-runs" data-machine-runs>
      <span>{runs}</span>
      {disk === null ? null : <span>{disk}</span>}
    </p>
  );
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

// How many features the project has queued: 0 when nothing is, or the queue is unknown.
function queuedCount(queue) {
  return queue !== null && typeof queue === "object" && isCount(queue.count) ? queue.count : 0;
}

// The runs of a project whose MR merged and whose clean-up has not finished, as its card lists them.
function waitingCleanups(list) {
  if (!Array.isArray(list)) return [];
  return list.filter((waiting) => waiting !== null && typeof waiting === "object" && typeof waiting.run === "string" && RUN_ID.test(waiting.run) && typeof waiting.feature === "string" && typeof waiting.review === "string");
}

// The commits on a project's target its remote does not have: how many, and the newest subjects. Given
// `push` ({token, projectId, onSignedOut}), a GitHub project's line offers Push <target>; any other
// provider's says to deliver by MR review instead. Once the push is done the line gives way to what it did,
// until a refresh brings other commits not pushed (another target tip): then those, with their own Push,
// come back. A refresh with nothing waiting ends the note.
function Unpushed({ unpushed, subjects = false, push = null }) {
  const [pushed, setPushed] = useState(null);
  const waiting = unpushed !== null && typeof unpushed === "object" && typeof unpushed.message === "string";
  useEffect(() => {
    if (!waiting) setPushed(null);
  }, [waiting]);
  if (!waiting) return null;
  const shown = typeof unpushed.sha === "string" ? unpushed.sha : JSON.stringify([unpushed.target, unpushed.count, unpushed.message, unpushed.subjects]);
  if (pushed !== null && pushed.of === shown) return <p className="run-action-note" role="status" data-push-done>{pushed.text}</p>;
  const listed = subjects && Array.isArray(unpushed.subjects) ? unpushed.subjects.filter((subject) => typeof subject === "string") : [];
  const target = typeof unpushed.target === "string" && unpushed.target !== "" ? unpushed.target : null;
  return (
    <div className="unpushed" role="note" data-unpushed>
      {listed.length > 0 ? (
        <details className="unpushed-details" data-unpushed-details>
          <summary className="unpushed-warning"><Icon name="alert" size={16} /><span>{unpushed.message}</span><Icon name="chevronDown" size={16} /></summary>
          <ul className="unpushed-subjects">{listed.map((subject, at) => <li key={at}>{subject}</li>)}</ul>
        </details>
      ) : <p className="unpushed-warning"><Icon name="alert" size={16} /><span>{unpushed.message}</span></p>}
      {push === null || target === null ? null : unpushed.provider === "github"
        ? <PushTarget key={shown} target={target} push={push} onPushed={(text) => setPushed({ text, of: shown })} />
        : <p className="muted" data-push-gitlab>GitLab: deliver by MR review</p>}
    </div>
  );
}

// While `active`, Tab and Shift+Tab wrap around inside `box`, so the page under a popup is out of reach.
function useTabInside(box, active = true) {
  useEffect(() => {
    if (!active) return undefined;
    const keepFocus = (event) => {
      if (event.key !== "Tab" || box.current === null) return;
      const stops = [...box.current.querySelectorAll("button, input, textarea, select, a[href]")].filter((el) => !el.disabled && el.tabIndex >= 0);
      if (stops.length === 0) {
        // Everything inside is off (a send is out): the focus rests on the popup itself.
        event.preventDefault();
        box.current.focus();
        return;
      }
      const first = stops[0];
      const last = stops[stops.length - 1];
      const at = document.activeElement;
      // The popup itself holding the focus (a send moved it there) is an edge both ways.
      const edge = at === box.current || !box.current.contains(at);
      if (event.shiftKey && (at === first || edge)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (at === last || edge)) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", keepFocus);
    return () => document.removeEventListener("keydown", keepFocus);
  }, [active]);
}

// Most commits a push confirmation lists before "and N more".
const PUSH_LISTED = 50;

// Push <target>: reads what a push would send and why it may not, then asks with the remote, the branch, the
// range and each commit (its run, or not from orq), and sends the commit it showed. Only a passkey session
// may push, so signed in with the token the button is off and says so.
function PushTarget({ target, push, onPushed }) {
  const [phase, setPhase] = useState("idle");
  const [state, setState] = useState(null);
  const [note, setNote] = useState("");
  const path = "/api/projects/" + encodeURIComponent(push.projectId) + "/push-target";
  const session = push.token === SESSION;
  const ask = async () => {
    setNote("");
    setPhase("checking");
    try {
      const response = await get(path, push.token);
      if (response.status === 401) {
        push.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok && body !== null && typeof body === "object") {
        setState(body);
        setPhase("asking");
        return;
      }
      setNote(body !== null && typeof body.error === "string" ? body.error : "The push could not be checked.");
    } catch (unreachable) {
      setNote("The server could not be reached.");
    }
    setPhase("idle");
  };
  const send = async () => {
    setPhase("sending");
    let text = "The push could not be completed.";
    try {
      const response = await actionRequest(path, push.token, { sha: state.sha });
      if (response.status === 401) {
        push.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok && body !== null && isCount(body.count)) {
        onPushed("Pushed " + body.count + (body.count === 1 ? " commit" : " commits") + " to " + state.remote + "/" + target + ".");
        return;
      }
      if (response.status === 403) text = body !== null && typeof body.error === "string" ? body.error : "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("asking");
  };
  const cancel = () => {
    setPhase("idle");
    setState(null);
    setNote("");
  };
  const open = (phase === "asking" || phase === "sending") && state !== null;
  const busy = phase === "sending";
  useEffect(() => {
    if (!open || busy) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape" && !event.defaultPrevented) cancel();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, busy]);
  // Opening moves the focus to Cancel, Tab stays inside the popup, and closing gives it back to Push.
  const box = useRef(null);
  const button = useRef(null);
  const wasOpen = useRef(false);
  useEffect(() => {
    if (open && !wasOpen.current) {
      const cancelButton = box.current === null ? null : box.current.querySelector("[data-cancel]");
      if (cancelButton !== null) cancelButton.focus();
    } else if (!open && wasOpen.current && button.current !== null && button.current.isConnected) {
      button.current.focus();
    }
    wasOpen.current = open;
  }, [open]);
  // Sending turns off the focused Push N commits: the focus moves to the popup rather than the page.
  useEffect(() => {
    if (!busy || box.current === null) return;
    const at = document.activeElement;
    if (at === null || !box.current.contains(at) || at.disabled) box.current.focus();
  }, [busy]);
  useTabInside(box, open);
  const error =note !== "" ? <span role="status" className="run-action-note error-text" data-push-error>{note}</span> : null;
  // The confirmation is a popup over the page, so the card or line the button sits in keeps its size.
  return (
    <div className="push-target">
      <button type="button" className="primary-button" ref={button} data-push-target disabled={!session || phase !== "idle"} onClick={ask}>{phase === "checking" ? <span className="spinner" aria-hidden="true" data-push-checking /> : <Icon name="arrowUp" size={16} />}{"Push " + target}</button>
      {session ? null : <p className="muted" data-push-needs-passkey>Pushing needs a passkey sign-in.</p>}
      {phase === "checking" ? <span role="status" className="sr-only">Checking…</span> : null}
      {open ? (
        <div className="feature-modal" data-push-modal onClick={(event) => { if (event.target === event.currentTarget && !busy) cancel(); }}>
          <div className="feature-modal-box" ref={box} tabIndex={-1} role="dialog" aria-modal="true" aria-label={"Push " + target}>
            <div className="feature-modal-head">
              <h2>{"Push " + target}</h2>
              <button type="button" className="icon-button" data-modal-close aria-label="Close" disabled={busy} onClick={cancel}><Icon name="close" size={18} /></button>
            </div>
            <PushConfirm state={state} target={target} projectId={push.projectId} busy={busy} onConfirm={send} onCancel={cancel} />
            {error}
          </div>
        </div>
      ) : error}
    </div>
  );
}

// What a push would send, and Push N commits; or why it is off, with only Cancel.
function PushConfirm({ state, target, projectId, busy, onConfirm, onCancel }) {
  const remote = typeof state.remote === "string" && state.remote !== "" ? state.remote : "origin";
  const commits = Array.isArray(state.commits) ? state.commits.filter((commit) => commit !== null && typeof commit === "object" && typeof commit.short === "string" && typeof commit.subject === "string") : [];
  const shown = commits.slice(0, PUSH_LISTED);
  const more = (isCount(state.more) ? state.more : 0) + (commits.length - shown.length);
  const reason = state.remote_actions !== true ? "Remote actions are off on this server." : typeof state.reason === "string" && state.reason !== "" ? state.reason : typeof state.sha !== "string" || shown.length === 0 ? "Nothing is waiting to be pushed." : null;
  const count = isCount(state.count) ? state.count : commits.length;
  const commitCount = count + (count === 1 ? " commit" : " commits");
  const runHref = (run) => "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(run);
  return (
    <div className="confirm push-confirm" role="group" aria-label={"Push " + target + " to " + remote} data-push-confirm>
      <p className="confirm-question">{"Push " + target + " to " + remote + "/" + target + "?"}</p>
      <p className="muted">{"Remote: " + remote + ", branch: " + target + ", range: " + remote + "/" + target + ".." + target}</p>
      {shown.length > 0 ? (
        <ul className="push-commits">
          {shown.map((commit, at) => (
            <li key={at} data-push-commit>
              <code>{commit.short}</code>
              <span>{" " + commit.subject + " "}</span>
              {typeof commit.run === "string" && RUN_ID.test(commit.run)
                ? <a href={runHref(commit.run)} data-push-commit-run onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); onCancel(); navigate(runHref(commit.run)); } }}>{commit.run}</a>
                : <em className="muted">not from orq</em>}
            </li>
          ))}
          {more > 0 ? <li className="muted">{"and " + more + " more"}</li> : null}
        </ul>
      ) : null}
      {reason !== null ? <p className="error-text" role="note" data-push-reason>{reason}</p> : null}
      <div className="confirm-actions">
        {reason === null ? (
          <button type="button" className="primary-button" data-push-send disabled={busy} aria-busy={busy} onClick={onConfirm}>
            {busy ? <><span className="spinner" aria-hidden="true" data-push-sending />Pushing…</> : "Push " + commitCount}
          </button>
        ) : null}
        <button type="button" data-cancel disabled={busy} onClick={onCancel}>Cancel</button>
        {busy ? <span role="status">{"Pushing " + commitCount + " to " + remote + "/" + target + "…"}</span> : null}
      </div>
    </div>
  );
}

function ProjectCard({ project, selected = false, pinned = false, onPin = null, push = null }) {
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
          {Number.isInteger(project.max_active_runs) && project.active_runs !== null
            ? <CardStat icon="play" value={project.active_runs + " of " + project.max_active_runs} label="runs active" to={base + "orchestrations"} id={project.id + "/runs"} />
            : <CardStat icon="play" value={project.active_runs} label="Active runs" to={base + "orchestrations"} id={project.id + "/runs"} />}
          {project.reviews_waiting > 0
            ? <CardStat icon="doc" value={project.reviews_waiting} label="Reviews waiting" to={base + "reviews"} id={project.id + "/reviews"} />
            : <CardStat icon="clock" value={queuedCount(project.queue)} label="Queued" to={base + "queue"} id={project.id + "/queue"} />}
        </div>
      ) : (
        <em>Unavailable</em>
      )}
      {typeof project.queue?.target_pull_warning === "string" ? (
        <p className="error-text" role="note" data-card-target-pull><Icon name="alert" size={16} />{project.queue.target_pull_warning}</p>
      ) : null}
      {waitingCleanups(project.cleanup).map((waiting) => (
        <button key={waiting.run} type="button" className="card-cleanup" data-card-cleanup={waiting.run} onClick={() => navigate(base + "orchestrations/" + encodeURIComponent(waiting.run))}>
          <Icon name="check" size={16} />{waiting.feature + " merged in " + waiting.review + ": clean up"}
        </button>
      ))}
      <Unpushed unpushed={project.unpushed} push={push} />
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

const API_TODO = "/api/todo";
// How many Todo items show before "Show all".
const TODO_SHOWN = 10;
const ACTIONS_OFF = "Remote actions are off, so nothing can be added. Turn them on in Settings.";

function isTodo(item) {
  return item !== null && typeof item === "object" && Number.isInteger(item.id) && item.id > 0 && typeof item.name === "string" && typeof item.description === "string"
    && Number.isInteger(item.created_at) && Number.isInteger(item.version);
}

function parseTodos(data) {
  return data && Array.isArray(data.todos) && data.todos.every(isTodo) ? { items: data.todos, actions: data.remote_actions === true } : null;
}

// Seconds since the epoch as the UTC time a Moment shows.
function epochTime(seconds) {
  return new Date(seconds * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");
}

// Posts body to path; null when it went through, otherwise what to say. A refused sign-in signs out (undefined).
async function todoSend(path, token, onSignedOut, body, fallback) {
  try {
    const response = await actionRequest(path, token, body);
    if (response.status === 401) {
      onSignedOut();
      return undefined;
    }
    const reply = await jsonOrNull(response);
    if (response.ok) return null;
    if (response.status === 403) return "Not allowed.";
    return reply !== null && typeof reply.error === "string" ? reply.error : fallback;
  } catch (unreachable) {
    return "The server could not be reached.";
  }
}

// A name and a description as the Todo list keeps them.
function TodoFields({ name, description, onName, onDescription }) {
  return (
    <>
      <label className="field"><span className="field-label">Name</span><input type="text" data-todo-name value={name} maxLength={200} onChange={(event) => onName(event.target.value)} /></label>
      <label className="field field-wide"><span className="field-label">Description</span><textarea data-todo-description rows={4} value={description} onChange={(event) => onDescription(event.target.value)} /></label>
    </>
  );
}

// One Todo item: its name, two lines of its description and when it was added, with Move to project, Edit and
// Delete while remote actions are on. Edit opens its popup (onEdit); Delete asks first, in the card.
function TodoItem({ item, actions, token, onSignedOut, onChanged, onMove, onDraft, onEdit }) {
  const [mode, setMode] = useState("view");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const remove = async () => {
    setBusy(true);
    setNote("");
    const why = await todoSend(API_TODO + "/" + item.id + "/delete", token, onSignedOut, {}, "The item could not be deleted.");
    if (why === undefined) return;
    setBusy(false);
    if (why === null) {
      setMode("view");
      onChanged();
    } else {
      setNote(why);
    }
  };
  return (
    <li className="todo-item card" data-todo-item={item.id}>
      <div className="todo-item-head">
        <strong className="todo-item-name">{item.name}</strong>
        <span className="todo-item-when muted">{"Added "}<Moment at={epochTime(item.created_at)} /></span>
      </div>
      {item.description !== "" ? <p className="todo-item-description">{item.description}</p> : null}
      {actions && mode === "view" ? (
        <div className="todo-item-actions">
          <button type="button" data-todo-move onClick={() => onMove(item)}><Icon name="chevronRight" size={16} />Move to project</button>
          <button type="button" data-todo-draft onClick={() => onDraft(item)}><Icon name="doc" size={16} />Draft with orq</button>
          <button type="button" data-todo-edit onClick={() => onEdit(item)}><Icon name="pencil" size={16} />Edit</button>
          <button type="button" data-todo-delete onClick={() => setMode("delete")}><Icon name="close" size={16} />Delete</button>
        </div>
      ) : null}
      {mode === "delete" ? (
        <Confirm question="Delete this Todo item?" detail={item.name} answer="Yes, delete" tone="danger" busy={busy} onConfirm={remove} onCancel={() => { setNote(""); setMode("view"); }} />
      ) : null}
      {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
    </li>
  );
}

// A Todo item's Edit, as a popup like Add feature: its name and description, Save (sending the version it read)
// and Cancel. A failed save keeps the popup and what was typed; Escape, Close, Cancel and the backdrop close it
// unsaved unless a save is out.
function TodoEditDialog({ item, token, onSignedOut, onClose, onSaved }) {
  const [name, setName] = useState(item.name);
  const [description, setDescription] = useState(item.description);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const box = useRef(null);
  useEffect(() => {
    const field = box.current === null ? null : box.current.querySelector("[data-todo-name]");
    if (field !== null) field.focus();
  }, []);
  useEffect(() => {
    if (busy) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape" && !event.defaultPrevented) onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [busy]);
  const save = async () => {
    setBusy(true);
    setNote("");
    const why = await todoSend(API_TODO + "/" + item.id, token, onSignedOut, { name, description, version: item.version }, "The item could not be saved.");
    if (why === undefined) return;
    setBusy(false);
    if (why === null) onSaved();
    else setNote(why);
  };
  useTabInside(box);
  return (
    <div className="feature-modal" data-feature-modal data-todo-edit-dialog onClick={(event) => { if (event.target === event.currentTarget && !busy) onClose(); }}>
      <div className="feature-modal-box" ref={box} role="dialog" aria-modal="true" aria-labelledby="todo-edit-title">
        <div className="feature-modal-head">
          <h2 id="todo-edit-title">Edit Todo</h2>
          <button type="button" className="icon-button" data-modal-close aria-label="Close" disabled={busy} onClick={onClose}><Icon name="close" size={18} /></button>
        </div>
        <div className="feature-editor-fields" role="group" aria-label={"Edit " + item.name}>
          <TodoFields name={name} description={description} onName={setName} onDescription={setDescription} />
          <div className="feature-editor-actions">
            <button type="button" className="primary-button" data-todo-save disabled={busy || name.trim() === ""} onClick={save}><Icon name="check" size={16} />Save</button>
            <button type="button" data-cancel disabled={busy} onClick={onClose}>Cancel</button>
          </div>
          {busy ? <span role="status">Sending…</span> : null}
          {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
        </div>
      </div>
    </div>
  );
}

// The Todo list above the projects: always shown, newest first, TODO_SHOWN items until "Show all".
function TodoList({ todo, token, onSignedOut, onChanged, onMove, onDraft }) {
  const [all, setAll] = useState(false);
  // The item whose Edit popup is open, or null.
  const [editing, setEditing] = useState(null);
  // The item whose Edit button gets the focus back once its popup has closed.
  const returnTo = useRef(null);
  useEffect(() => {
    if (editing !== null || returnTo.current === null) return;
    const button = document.querySelector("[data-todo-item=\"" + returnTo.current + "\"] [data-todo-edit]");
    returnTo.current = null;
    if (button !== null) button.focus();
  }, [editing]);
  const closeEdit = () => {
    returnTo.current = editing.id;
    setEditing(null);
  };
  let body;
  if (todo === null) body = <p className="muted">Loading the Todo list…</p>;
  else if (typeof todo === "string") body = <p className="muted">{todo}</p>;
  else if (todo.items.length === 0) body = <p className="muted todo-empty">Nothing to do. Add feature puts ideas here.</p>;
  else {
    const shown = all ? todo.items : todo.items.slice(0, TODO_SHOWN);
    body = (
      <>
        <ul className="todo-list">
          {shown.map((item) => <TodoItem key={item.id} item={item} actions={todo.actions} token={token} onSignedOut={onSignedOut} onChanged={onChanged} onMove={onMove} onDraft={onDraft} onEdit={setEditing} />)}
        </ul>
        {todo.items.length > TODO_SHOWN ? <button type="button" className="todo-show-all" data-todo-show-all aria-expanded={all ? "true" : "false"} onClick={() => setAll(!all)}>{all ? "Show fewer" : "Show all (" + todo.items.length + ")"}</button> : null}
      </>
    );
  }
  return (
    <section className="todo" data-todo>
      <h2 className="section-title">Todos</h2>
      {body}
      {editing !== null ? <TodoEditDialog key={editing.id} item={editing} token={token} onSignedOut={onSignedOut} onClose={closeEdit} onSaved={() => { closeEdit(); onChanged(); }} /> : null}
    </section>
  );
}

// Add feature (item null) or a Todo item's Move to project, as a popup. Add's Project choice starts at Todo, which
// keeps a name and a description; a project opens the full feature editor and saves a draft there. A move offers
// only projects, prefills the editor from the item and can ask for the draft to be queued once it is approved.
function AddFeatureDialog({ projects, item = null, todoActions, token, onSignedOut, onClose, onFinished }) {
  const moving = item !== null;
  const [target, setTarget] = useState("");
  const [ledger, setLedger] = useState(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [queue, setQueue] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const choices = projects.filter((project) => project.available).sort((a, b) => a.name.localeCompare(b.name));
  const project = choices.find((entry) => entry.id === target) || null;
  useEffect(() => {
    if (busy) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape" && !event.defaultPrevented) onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [busy]);
  useEffect(() => {
    setLedger(null);
    if (target === "") return undefined;
    const controller = new AbortController();
    (async () => {
      let next = "The project's features could not be read.";
      try {
        const response = await get(API_PROJECTS + "/" + encodeURIComponent(target) + "/features", token, controller.signal);
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        const parsed = response.ok ? parseFeatures(await jsonOrNull(response)) : null;
        if (parsed !== null) next = parsed;
      } catch (unreachable) {
        next = "The server could not be reached.";
      }
      if (!controller.signal.aborted) setLedger(next);
    })();
    return () => controller.abort();
  }, [target]);
  const saveTodo = async () => {
    setBusy(true);
    setNote("");
    const why = await todoSend(API_TODO, token, onSignedOut, { name, description }, "The item could not be added.");
    if (why === undefined) return;
    if (why === null) {
      onFinished("Added to Todo");
      return;
    }
    setBusy(false);
    setNote(why);
  };
  const todoBlocked = todoActions ? null : ACTIONS_OFF;
  let form = null;
  if (target === "" && !moving) {
    form = (
      <div className="feature-editor-fields" role="group" aria-label="Add to Todo">
        <TodoFields name={name} description={description} onName={setName} onDescription={setDescription} />
        {todoBlocked !== null ? <p className="field-hint muted" data-add-blocked>{todoBlocked}</p> : null}
        <div className="feature-editor-actions">
          <button type="button" className="primary-button" data-todo-save disabled={busy || todoBlocked !== null || name.trim() === ""} onClick={saveTodo}><Icon name="check" size={16} />Save</button>
          <button type="button" data-cancel disabled={busy} onClick={onClose}>Cancel</button>
        </div>
        {busy ? <span role="status">Sending…</span> : null}
        {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
      </div>
    );
  } else if (project !== null && ledger === null) {
    form = <p role="status">Loading the project's features…</p>;
  } else if (project !== null && typeof ledger === "string") {
    form = <p role="status" className="error-text">{ledger}</p>;
  } else if (project !== null) {
    const control = { token, projectId: project.id, onSignedOut, onDone: () => {} };
    const queueBox = moving ? (
      <label className="depends-option" data-queue-option><input type="checkbox" data-queue-when-approved checked={queue} disabled={busy} onChange={(event) => setQueue(event.target.checked)} />Queue once approved</label>
    ) : null;
    form = (
      <FeatureEditor
        key={project.id}
        embedded
        features={ledger.rows}
        control={control}
        initial={moving ? { name: item.name, description: item.description } : null}
        target={moving ? { path: API_TODO + "/" + item.id + "/move", body: (fields) => ({ project: project.id, fields, version: item.version, queue_when_approved: queue }) } : null}
        doneText={(moving ? "Moved to " : "Added to ") + project.name + " as a draft"}
        onFinished={onFinished}
        onClose={onClose}
        onSending={setBusy}
        blocked={ledger.actions ? null : ACTIONS_OFF}
        extra={queueBox}
      />
    );
  }
  const title = moving ? "Move to project" : "Add feature";
  return (
    <div className="feature-modal" data-feature-modal>
      <div className="feature-modal-box" role="dialog" aria-modal="true" aria-labelledby="add-feature-title">
        <div className="feature-modal-head">
          <h2 id="add-feature-title">{title}</h2>
          <button type="button" className="icon-button" data-modal-close aria-label="Close" disabled={busy} onClick={onClose}><Icon name="close" size={18} /></button>
        </div>
        {moving ? <p className="muted">{item.name}</p> : null}
        <div className="field">
          <span className="field-label">Project</span>
          <ProjectChooser choices={moving ? choices : [{ id: "", name: "Todo (no project)" }, ...choices]} value={target} placeholder="Choose a project" disabled={busy} onChoose={setTarget} buttonProps={{ "data-add-target": true }} />
        </div>
        {form}
      </div>
    </div>
  );
}

// The harnesses orq can run read-only for a planning run, claude first; a planning run starts on claude with haiku.
const PLAN_HARNESSES = ["claude", "pi"];
const PLAN_MODEL = "haiku";

// The model a planning run starts on with `option`: claude's haiku when it offers it, else its first model.
function planModel(option) {
  return option.harness === "claude" && option.models.includes(PLAN_MODEL) ? PLAN_MODEL : option.models[0];
}

// Draft with orq, as a popup: a Todo item's idea (with a project to choose) or a draft feature to improve (its
// project fixed), the harness and model that draft it, plus optional notes. Start begins a planning run and
// opens its page.
function DraftWithOrqDialog({ projects = [], item = null, draft = null, token, onSignedOut, onClose }) {
  const improving = draft !== null;
  const [target, setTarget] = useState(improving ? draft.project : "");
  const [notes, setNotes] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  // The harnesses offered here (null while they are read) and the one chosen with its model.
  const [options, setOptions] = useState(null);
  const [agent, setAgent] = useState(null);
  const choices = projects.filter((project) => project.available).sort((a, b) => a.name.localeCompare(b.name));
  useEffect(() => {
    const controller = new AbortController();
    (async () => {
      let listed = null;
      try {
        const response = await get(API_SETTINGS + "/options", token, controller.signal);
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        listed = response.ok ? parseOptions(await jsonOrNull(response)) : null;
      } catch (unreachable) {
        listed = null;
      }
      if (controller.signal.aborted) return;
      const offered = PLAN_HARNESSES.map((harness) => (listed || []).find((option) => option.harness === harness)).filter(Boolean);
      setOptions(offered);
      if (offered.length === 0) setNote("There are no harnesses to choose from.");
      else setAgent({ harness: offered[0].harness, model: planModel(offered[0]) });
    })();
    return () => controller.abort();
  }, []);
  const current = agent === null ? null : options.find((option) => option.harness === agent.harness);
  const pickHarness = (harness) => {
    const option = options.find((each) => each.harness === harness);
    if (option) setAgent({ harness, model: planModel(option) });
  };
  useEffect(() => {
    if (busy) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape" && !event.defaultPrevented) onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [busy]);
  const start = async () => {
    setBusy(true);
    setNote("");
    const path = improving
      ? API_PROJECTS + "/" + encodeURIComponent(draft.project) + "/features/" + encodeURIComponent(draft.name) + "/draft-with-claude"
      : API_TODO + "/" + item.id + "/draft-with-claude";
    const chosen = { harness: agent.harness, model: agent.model };
    const body = improving ? { notes, ...chosen } : { project: target, version: item.version, notes, ...chosen };
    let why = "The planning run could not be started.";
    try {
      const response = await actionRequest(path, token, body);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const reply = await jsonOrNull(response);
      if (response.ok && reply !== null && typeof reply.run === "string") {
        onClose();
        navigate(runHref(target, reply.run));
        return;
      }
      if (response.status === 403) why = "Not allowed.";
      else if (reply !== null && typeof reply.error === "string") why = reply.error;
    } catch (unreachable) {
      why = "The server could not be reached.";
    }
    setBusy(false);
    setNote(why);
  };
  const title = improving ? "Improve draft" : "Draft with orq";
  return (
    <div className="feature-modal" data-feature-modal data-plan-dialog>
      <div className="feature-modal-box" role="dialog" aria-modal="true" aria-labelledby="plan-title">
        <div className="feature-modal-head">
          <h2 id="plan-title">{title}</h2>
          <button type="button" className="icon-button" data-modal-close aria-label="Close" disabled={busy} onClick={onClose}><Icon name="close" size={18} /></button>
        </div>
        {improving ? (
          <p className="muted">{draft.project + " · " + draft.name}</p>
        ) : (
          <>
            <div className="field">
              <span className="field-label">Project</span>
              <ProjectChooser choices={choices} value={target} placeholder="Choose a project" disabled={busy} onChoose={setTarget} buttonProps={{ "data-plan-project": true }} />
            </div>
            <div className="plan-idea" data-plan-idea>
              <strong>{item.name}</strong>
              {item.description !== "" ? <p className="todo-item-description">{item.description}</p> : null}
            </div>
          </>
        )}
        {agent !== null ? (
          <>
            <label className="field">
              <span className="field-label">Harness</span>
              <select data-plan-harness value={agent.harness} disabled={busy} onChange={(event) => pickHarness(event.target.value)}>
                {options.map((option) => <option key={option.harness} value={option.harness}>{option.harness}</option>)}
              </select>
            </label>
            <label className="field">
              <span className="field-label">Model</span>
              <select data-plan-model value={agent.model} disabled={busy} onChange={(event) => setAgent({ ...agent, model: event.target.value })}>
                {current.models.map((model) => <option key={model} value={model}>{model}</option>)}
              </select>
            </label>
          </>
        ) : options === null ? <span role="status">Reading the harnesses…</span> : null}
        <label className="field field-wide"><span className="field-label">Notes (optional)</span><textarea data-plan-notes rows={4} value={notes} disabled={busy} onChange={(event) => setNotes(event.target.value)} /></label>
        <div className="feature-editor-actions">
          <button type="button" className="primary-button" data-plan-start disabled={busy || target === "" || agent === null} onClick={start}><Icon name="play" size={16} />Start</button>
          <button type="button" data-cancel disabled={busy} onClick={onClose}>Cancel</button>
        </div>
        {busy ? <span role="status">Starting…</span> : null}
        {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
      </div>
    </div>
  );
}

// The Todo list as the Dashboard reads it: null while loading, what went wrong, or { items, actions }.
function useTodo(token, onSignedOut, reads) {
  const [todo, setTodo] = useState(null);
  useEffect(() => {
    const controller = new AbortController();
    (async () => {
      let next = "The Todo list could not be read.";
      try {
        const response = await get(API_TODO, token, controller.signal);
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        const parsed = response.ok ? parseTodos(await jsonOrNull(response)) : null;
        if (parsed !== null) next = parsed;
      } catch (unreachable) {
        next = "The Todo list could not be read.";
      }
      if (!controller.signal.aborted) setTodo(next);
    })();
    return () => controller.abort();
  }, [reads]);
  return todo;
}

// The fourth tile: how many Todo items there are ("–" while loading or when the list can't be read, the reason
// as its title). Pressing it opens Add feature.
function TodoStat({ todo, onAdd }) {
  const known = todo !== null && typeof todo === "object";
  const value = known ? todo.items.length : "–";
  const label = known && todo.items.length === 1 ? "Todo" : "Todos";
  const name = (known ? value + " " + label : label) + ", add feature";
  return (
    <button type="button" className="stat stat-link stat-add" data-dashboard-add aria-label={name} title={typeof todo === "string" ? todo : undefined} onClick={onAdd}>
      <span className="stat-icon tone-accent"><Icon name="plus" size={24} /></span>
      <span className="stat-text">
        <span className="stat-value">{value}</span>
        <span className="stat-label">{label}</span>
      </span>
    </button>
  );
}

// The totals above the project cards; a project that could not be read adds nothing to the sums.
function Dashboard({ projects, machine = null, token, onSignedOut }) {
  const [pins, setPins] = useState(readPins);
  const [reads, setReads] = useState(0);
  const todo = useTodo(token, onSignedOut, reads);
  // The open Add feature popup ({ item: null }), or null when none is.
  const [adding, setAdding] = useState(null);
  const [notice, setNotice] = useState("");
  const finished = (text) => {
    setAdding(null);
    setNotice(text);
    setReads((n) => n + 1);
  };
  const sum = (key) => projects.reduce((total, project) => total + (project.available && Number.isInteger(project[key]) ? project[key] : 0), 0);
  const toggle = (id) => {
    const next = pins.includes(id) ? pins.filter((pin) => pin !== id) : [...pins, id];
    writePins(next);
    setPins(next);
  };
  const pinned = projects.filter((project) => pins.includes(project.id));
  const rest = projects.filter((project) => !pins.includes(project.id));
  const card = (project) => <ProjectCard key={project.id} project={project} selected={project.id === lastProject} pinned={pins.includes(project.id)} onPin={() => toggle(project.id)} push={{ token, projectId: project.id, onSignedOut }} />;
  return (
    <>
      <div className="stats">
        <Stat icon="layers" tone="tone-accent" value={projects.length} label="Projects" />
        <Stat icon="play" tone="tone-accent" value={sum("active_runs")} label="Active runs" to="/orchestrations" link="runs" />
        <Stat icon="doc" tone="tone-failed" value={sum("reviews_waiting")} label="Reviews waiting" to="/reviews" link="reviews" />
        <TodoStat todo={todo} onAdd={() => { setNotice(""); setAdding({ item: null }); }} />
      </div>
      {notice !== "" ? <p role="status" className="run-action-note" data-dashboard-notice>{notice}</p> : null}
      {machine === null ? null : <MachineRuns machine={machine} />}
      {projects.length === 0 ? <p>No projects found.</p> : null}
      {adding !== null ? <AddFeatureDialog projects={projects} item={adding.item} todoActions={todo !== null && typeof todo === "object" && todo.actions} token={token} onSignedOut={onSignedOut} onClose={() => setAdding(null)} onFinished={finished} /> : null}
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

// The Todo list's own page: newest first, with each item's Move to project, Draft with orq, Edit and Delete.
// The projects are read because the move and draft popups offer them. Refresh reads the list again too.
function TodosPage({ projects, reloads, token, onSignedOut }) {
  const [reads, setReads] = useState(0);
  const todo = useTodo(token, onSignedOut, reloads + ":" + reads);
  // The open Move to project popup ({ item }), or null when none is.
  const [adding, setAdding] = useState(null);
  // The Todo item whose Draft with orq popup is open, or null.
  const [drafting, setDrafting] = useState(null);
  const [notice, setNotice] = useState("");
  const reread = () => setReads((n) => n + 1);
  const finished = (text) => {
    setAdding(null);
    setNotice(text);
    reread();
  };
  return (
    <>
      {notice !== "" ? <p role="status" className="run-action-note" data-dashboard-notice>{notice}</p> : null}
      <TodoList todo={todo} token={token} onSignedOut={onSignedOut} onChanged={reread} onMove={(item) => { setNotice(""); setAdding({ item }); }} onDraft={(item) => { setNotice(""); setDrafting(item); }} />
      {drafting !== null ? <DraftWithOrqDialog projects={projects} item={drafting} token={token} onSignedOut={onSignedOut} onClose={() => setDrafting(null)} /> : null}
      {adding !== null ? <AddFeatureDialog projects={projects} item={adding.item} todoActions={todo !== null && typeof todo === "object" && todo.actions} token={token} onSignedOut={onSignedOut} onClose={() => setAdding(null)} onFinished={finished} /> : null}
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
  if (!(ok && settings.implementer !== undefined && settings.reviewer !== undefined)) return null;
  return { ...settings, server: parseServer(data.server) };
}

// The server's own status beside the settings: its build, the installed one, and any restart waiting for the
// runs to end; null when the server does not say.
function parseServer(server) {
  if (server === null || typeof server !== "object" || typeof server.running !== "string") return null;
  return {
    running: server.running,
    installed: typeof server.installed === "string" ? server.installed : null,
    newer: server.newer_installed === true,
    pending: server.restart_pending === true,
    waiting: typeof server.waiting_for === "number" ? server.waiting_for : 0,
    supervision: typeof server.supervision === "string" ? server.supervision : null,
    // Under systemd, each harness its PATH lacks: runs started from here would fail.
    harnessWarnings: Array.isArray(server.harness_warnings) ? server.harness_warnings.filter((w) => typeof w === "string") : [],
  };
}

// The server's builds on Settings, and its restart. Restart when idle needs remote actions on and a passkey
// session, and asks first; the server restarts into the installed orq once no run is active. While a restart
// waits, it says for how many runs, and Cancel drops it.
function ServerCard({ server, on, token, onSignedOut, onChanged }) {
  const [asking, setAsking] = useState(false);
  const [sending, setSending] = useState(false);
  const [note, setNote] = useState("");
  const send = async (path) => {
    setSending(true);
    setNote("");
    let text = "The server could not be asked.";
    try {
      const response = await actionRequest(path, token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.ok) {
        setSending(false);
        setAsking(false);
        onChanged();
        return;
      }
      const reply = await response.json().catch(() => null);
      if (reply !== null && typeof reply.error === "string") text = reply.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setSending(false);
    setAsking(false);
    setNote(text);
  };
  const runs = (count) => count + (count === 1 ? " run" : " runs");
  return (
    <section className="card settings-card" data-server>
      <SettingsHead icon="cpu">Server</SettingsHead>
      <dl className="settings-fields">
        <div className="settings-row"><dt>Running</dt><dd>{server.running}</dd></div>
        <div className="settings-row"><dt>Installed</dt><dd>{server.installed ?? <span className="muted">No installed orq found</span>}</dd></div>
        {server.supervision !== null ? <div className="settings-row" data-supervision><dt>Restarts</dt><dd>{server.supervision}</dd></div> : null}
      </dl>
      {server.harnessWarnings.map((warning) => <p key={warning} className="settings-block" role="status" data-harness-warning>{warning}</p>)}
      {server.newer ? <p className="settings-block" role="status" data-newer-installed>A newer orq is installed</p> : null}
      {server.pending ? (
        <div className="settings-row" data-restart-pending>
          <span role="status">{"Restart pending: waiting for " + runs(server.waiting)}</span>
          <button type="button" data-restart-cancel disabled={sending} onClick={() => send(API_SERVER_RESTART + "/cancel")}>Cancel</button>
        </div>
      ) : on && token === SESSION ? (
        <div className="settings-row">
          <span>Restart when idle</span>
          {asking ? (
            <Confirm question="Restart orq once no run is active? It restarts into the installed orq." answer="Yes, restart when idle" tone="primary" busy={sending} onConfirm={() => send(API_SERVER_RESTART)} onCancel={() => setAsking(false)} />
          ) : (
            <button type="button" className="primary-button" data-restart-server onClick={() => { setNote(""); setAsking(true); }}>Restart when idle</button>
          )}
        </div>
      ) : null}
      {note !== "" && <p role="alert">{note}</p>}
    </section>
  );
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
        <strong>{passkey.label}</strong> {passkey.this_device === true && <StatusPill label="This device" icon="monitor" tone="neutral" />} <code>{passkey.id}</code>{" "}
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

// Web push. Every /api/push route needs a passkey session (or the token where it stands in for one); a
// change carries the session's CSRF token. Resolves { status, ok, data } with the reply's JSON (or null).
async function pushRequest(token, method, tail, body) {
  const headers = token === SESSION ? {} : { Authorization: "Bearer " + token };
  if (method !== "GET" && token === SESSION) {
    const status = await authStatus();
    if (status !== null && typeof status.csrf === "string") headers["X-CSRF-Token"] = status.csrf;
  }
  const options = { method, headers, cache: "no-store", referrerPolicy: "no-referrer" };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(body);
  }
  const response = await fetch("/api/push/" + tail, options);
  return { status: response.status, ok: response.ok, data: await jsonOrNull(response) };
}

// The server's reason for a refusal when it gave one, otherwise `fallback`.
function pushRefusal(reply, fallback) {
  const data = reply && reply.data;
  return data !== null && typeof data === "object" && typeof data.error === "string" && data.error !== "" ? data.error : fallback;
}

const PUSH_UNSUPPORTED = "This browser cannot show notifications from this page.";
const PUSH_DENIED = "Notifications are blocked for this site. Allow them in the browser's site settings, then try again.";
const PUSH_FAILED = "Notifications could not be turned on for this device.";
const PUSH_OFF_FAILED = "Notifications could not be turned off for this device.";
const PUSH_KIND_LABELS = { needs_input: "Needs your input", failed: "Failed", stalled: "Stalled", review: "Review ready or out of date", merged: "MR merged", finished: "Finished", started: "Started", queue_paused: "Queue paused", queue_finished: "Queue finished", restarted: "Orq restarted" };

function pushSupported() {
  return window.isSecureContext === true && !!navigator.serviceWorker && typeof window.PushManager === "function" && !!window.Notification;
}

// A short name for this device from its user agent, such as "Firefox on Linux"; never the agent itself.
function deviceLabel(agent) {
  const ua = String(agent || "");
  const browser = /Edg\//.test(ua) ? "Edge" : /Firefox\/|FxiOS\//.test(ua) ? "Firefox" : /OPR\//.test(ua) ? "Opera" : /Chrome\/|CriOS\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "A browser";
  const system = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : /Windows/.test(ua) ? "Windows" : /CrOS/.test(ua) ? "ChromeOS" : /Macintosh|Mac OS X/.test(ua) ? "Mac" : /Linux/.test(ua) ? "Linux" : null;
  return system === null ? browser : browser + " on " + system;
}

// The bytes of a base64url text, as pushManager.subscribe takes the server's key.
function keyBytes(text) {
  const base = text.replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base + "=".repeat((4 - (base.length % 4)) % 4));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

// Asks the browser to show notifications, subscribes with the server's key and sends the subscription.
// Resolves null when this device is subscribed, otherwise the reason it is not; nothing is sent unless the
// browser can and may show notifications.
const PUSH_NO_WORKER = "This page's service worker did not start, so notifications cannot be set up. Reload the page and try again.";
const WORKER_READY_MS = 8000;

// The page's active service worker registration, or null when none is ready within WORKER_READY_MS:
// serviceWorker.ready never settles when no worker activates.
function workerReady() {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => resolve(null), WORKER_READY_MS);
    navigator.serviceWorker.ready.then(
      (registration) => {
        clearTimeout(timer);
        resolve(registration);
      },
      (failed) => {
        clearTimeout(timer);
        reject(failed);
      },
    );
  });
}

async function enablePush(token) {
  if (!pushSupported()) return PUSH_UNSUPPORTED;
  let permission = "denied";
  try {
    permission = await window.Notification.requestPermission();
  } catch (refused) {
    permission = "denied";
  }
  if (permission !== "granted") return PUSH_DENIED;
  try {
    const key = await pushRequest(token, "GET", "key");
    if (!key.ok || key.data === null || typeof key.data.public_key !== "string") return pushRefusal(key, PUSH_FAILED);
    const registration = await workerReady();
    if (registration === null) return PUSH_NO_WORKER;
    const subscription = (await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key.data.public_key) })).toJSON();
    const stored = await pushRequest(token, "POST", "subscriptions", { endpoint: subscription.endpoint, keys: subscription.keys, label: deviceLabel(navigator.userAgent) });
    return stored.ok ? null : pushRefusal(stored, PUSH_FAILED);
  } catch (failed) {
    return PUSH_FAILED;
  }
}

// The projects muted, read once the page opens: { muted, change }. `muted` is null while unread or when this
// page may not see the push settings; change(id, mute) resolves null once done and read again, otherwise
// the reason it was not.
function usePushMutes(token) {
  const [muted, setMuted] = useState(null);
  const load = useCallback(async () => {
    try {
      const reply = await pushRequest(token, "GET", "mutes");
      setMuted(reply.ok && reply.data !== null && Array.isArray(reply.data.muted) ? reply.data.muted.filter((id) => typeof id === "string") : null);
    } catch (unreachable) {
      setMuted(null);
    }
  }, [token]);
  useEffect(() => {
    load();
  }, [load]);
  const change = async (projectId, mute) => {
    const fallback = mute ? "The project could not be muted." : "The project could not be unmuted.";
    try {
      const reply = await pushRequest(token, mute ? "PUT" : "DELETE", "mutes/" + encodeURIComponent(projectId));
      if (!reply.ok) return pushRefusal(reply, fallback);
      await load();
      return null;
    } catch (unreachable) {
      return fallback;
    }
  };
  return { muted, change };
}

// This browser's push subscription, or null when it has none. Rejects when it cannot tell: no service
// worker ready within workerReady's wait, or the subscription could not be read.
async function currentSubscription() {
  if (!pushSupported()) return null;
  const registration = await workerReady();
  if (registration === null) throw new Error(PUSH_NO_WORKER);
  return (await registration.pushManager.getSubscription()) || null;
}

// A project's push mute on its settings page: changed at once, apart from Edit/Save. Not shown when this
// page may not see the push settings.
function ProjectNotificationsCard({ token, projectId }) {
  const mutes = usePushMutes(token);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  if (mutes.muted === null) return null;
  const muted = mutes.muted.includes(projectId);
  const toggle = async () => {
    setBusy(true);
    setNote("");
    const refused = await mutes.change(projectId, !muted);
    setNote(refused ?? "");
    setBusy(false);
  };
  return (
    <section className="card settings-card" data-project-notifications>
      <SettingsHead icon="alert">Notifications</SettingsHead>
      <div className="settings-block settings-actions">
        {muted ? <span data-push-muted={projectId}>Notifications muted</span> : <span>Push notifications on</span>}
        <button type="button" data-push-mute={projectId} disabled={busy} onClick={toggle}>
          {muted ? "Unmute" : "Mute notifications"}
        </button>
      </div>
      {note !== "" ? <p role="status" className="run-action-note" data-push-mute-note>{note}</p> : null}
    </section>
  );
}

const isPushDevice = (device) => device !== null && typeof device === "object" && Number.isInteger(device.id) && typeof device.label === "string";
const isPushKind = (kind) => kind !== null && typeof kind === "object" && typeof kind.kind === "string" && typeof kind.enabled === "boolean";

// One subscribed device: Send test shows "Sent" or the push service's error beside it, Remove forgets it.
function PushDevice({ device, token, onRemoved }) {
  const [result, setResult] = useState(null);
  const [sending, setSending] = useState(false);
  const test = async () => {
    setSending(true);
    setResult(null);
    try {
      const reply = await pushRequest(token, "POST", "subscriptions/" + device.id + "/test");
      if (reply.ok && reply.data !== null && reply.data.sent === true) setResult("Sent");
      else if (reply.ok && reply.data !== null && typeof reply.data.error === "string") setResult(reply.data.error);
      else setResult(pushRefusal(reply, "The test could not be sent."));
    } catch (unreachable) {
      setResult("The test could not be sent.");
    }
    setSending(false);
  };
  const remove = async () => {
    setSending(true);
    try {
      const reply = await pushRequest(token, "DELETE", "subscriptions/" + device.id);
      setSending(false);
      if (reply.ok) onRemoved();
      else setResult(pushRefusal(reply, "The device could not be removed."));
    } catch (unreachable) {
      setSending(false);
      setResult("The device could not be removed.");
    }
  };
  return (
    <li className="settings-row" data-push-device={device.id}>
      <strong>{device.label}</strong>{" "}
      <button type="button" data-push-test={device.id} disabled={sending} onClick={test}>
        Send test
      </button>
      <button type="button" className="danger-button" data-push-remove={device.id} disabled={sending} onClick={remove}>
        Remove
      </button>
      {result !== null && <span role="status" data-push-result={device.id}>{result}</span>}
    </li>
  );
}

// Settings, "Notifications on this device": an On/Off switch for this browser's subscription, the subscribed
// devices, and, while On, a switch per kind in a block of its own. Not shown
// when the server does not let this page see the push settings (no passkey session).
function PushCard({ token }) {
  const [devices, setDevices] = useState(null);
  const [kinds, setKinds] = useState(null);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [subscribed, setSubscribed] = useState(false);
  const load = async () => {
    try {
      const reply = await pushRequest(token, "GET", "subscriptions");
      if (reply.status === 401 || reply.status === 403 || reply.status === 404) setDevices("off");
      else setDevices(reply.ok && reply.data !== null && Array.isArray(reply.data.subscriptions) ? reply.data.subscriptions.filter(isPushDevice) : "failed");
    } catch (unreachable) {
      setDevices("failed");
    }
  };
  const loadKinds = async () => {
    try {
      const reply = await pushRequest(token, "GET", "kinds");
      setKinds(reply.ok && reply.data !== null && Array.isArray(reply.data.kinds) ? reply.data.kinds.filter(isPushKind) : "failed");
    } catch (unreachable) {
      setKinds("failed");
    }
  };
  useEffect(() => {
    let live = true;
    load();
    loadKinds();
    currentSubscription().then(
      (subscription) => {
        if (live) setSubscribed(subscription !== null);
      },
      () => {
        if (live) setSubscribed(false);
      },
    );
    return () => {
      live = false;
    };
  }, []);
  if (devices === null || devices === "off") return null;
  const enable = async () => {
    setBusy(true);
    setNote("");
    let refused = PUSH_FAILED;
    try {
      refused = await enablePush(token);
    } finally {
      setBusy(false);
    }
    setNote(refused === null ? "Notifications are on for this device." : refused);
    if (refused === null) {
      setSubscribed(true);
      load();
    }
  };
  // Off ends this browser's subscription only; its device row stays until Remove.
  const disable = async () => {
    setBusy(true);
    setNote("");
    let ended = false;
    try {
      const subscription = await currentSubscription();
      ended = subscription === null || (await subscription.unsubscribe()) === true;
    } catch (refused) {
      ended = false;
    }
    setBusy(false);
    if (ended) setSubscribed(false);
    else setNote(PUSH_OFF_FAILED);
  };
  const switchKind = async (kind, on) => {
    setBusy(true);
    setNote("");
    try {
      const reply = await pushRequest(token, "PUT", "kinds", { [kind]: on });
      if (reply.ok && reply.data !== null && Array.isArray(reply.data.kinds)) setKinds(reply.data.kinds.filter(isPushKind));
      else setNote(pushRefusal(reply, "The notification could not be switched."));
    } catch (unreachable) {
      setNote("The notification could not be switched.");
    }
    setBusy(false);
  };
  return (
    <section className="card settings-card push-settings" data-push-settings>
      <SettingsHead icon="alert">Notifications on this device</SettingsHead>
      <dl className="settings-fields">
        <div className="settings-row">
          <dt>Notify this device of runs that need you</dt>
          <dd className="settings-value">
            <button type="button" role="switch" className="follow-switch" aria-label="Notify this device of runs that need you" aria-checked={subscribed ? "true" : "false"} data-push-enable disabled={busy} onClick={subscribed ? disable : enable}>
              <span className="switch-track" aria-hidden="true"><span className="switch-thumb" /></span>
              <span>{subscribed ? "On" : "Off"}</span>
            </button>
          </dd>
        </div>
      </dl>
      {note !== "" && <p role="status" data-push-note>{note}</p>}
      {devices === "failed" ? (
        <>
          <p role="alert">The devices could not be listed.</p>
          <button type="button" onClick={load}>
            Try again
          </button>
        </>
      ) : (
        <ul className="settings-fields push-devices">
          {devices.length === 0 ? <li className="settings-row muted">No device is subscribed yet.</li> : null}
          {devices.map((device) => (
            <PushDevice key={device.id} device={device} token={token} onRemoved={load} />
          ))}
        </ul>
      )}
      {subscribed ? (
        <div className="settings-block push-kinds-block">
          <h3 className="settings-subhead">Notify me about</h3>
          {kinds === "failed" ? <p role="alert">The notification kinds could not be read.</p> : null}
          {Array.isArray(kinds) ? (
            <ul className="settings-fields push-kinds">
              {kinds.map((kind) => (
                <li key={kind.kind} className="settings-row">
                  <label>
                    <input type="checkbox" role="switch" data-push-kind={kind.kind} checked={kind.enabled} disabled={busy} onChange={() => switchKind(kind.kind, !kind.enabled)} />{" "}
                    {PUSH_KIND_LABELS[kind.kind] || kind.kind}
                  </label>
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}
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
              {line.outcome === "allowed" ? <StatusPill label={line.outcome} icon="check" tone="status-completed" /> : line.outcome === "refused" ? <StatusPill label={line.outcome} icon="close" tone="status-failed" /> : <StatusPill label={String(line.outcome)} icon="flag" tone="neutral" />}
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

// A refused reply's reason in the server's words, or `fallback` when it gives none.
async function reasonOf(response, fallback) {
  try {
    const reply = await response.json();
    return reply !== null && typeof reply.error === "string" ? reply.error : fallback;
  } catch (unreadable) {
    return fallback;
  }
}

function StorageCleanup({ token, onSignedOut }) {
  const [phase, setPhase] = useState("idle");
  const [preview, setPreview] = useState(null);
  const [lines, setLines] = useState([]);
  const [note, setNote] = useState("");
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
      {preview !== null && phase !== "loading" && preview.snapshots != null && isTextList(preview.snapshots.lines) && preview.snapshots.lines.length > 0 ? (
        <div className="settings-block">
          <span className="settings-label">Snapshots kept ({preview.snapshots.size}, never cleaned up)</span>
          <ul className="storage-preview">{preview.snapshots.lines.map((line, index) => <li key={index} data-snapshot-line>{line}</li>)}</ul>
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

// The Storage card's "Kept worktrees", while remote actions are on: every worktree orq made that a run left
// behind, grouped by project, read from /api/storage/worktrees when the card opens (a server without the
// route shows nothing). Each card's file list starts collapsed under its count. Delete asks first: a merged
// worktree holding only ignored files says they go with it, and any other names the kind of work it holds
// (not its paths) and waits for its branch name typed back, then sends force. "Delete all
// merged" removes the safe ones together. A refusal, such as a worktree changed since it was read, is shown
// in the server's words.
const API_WORKTREES = "/api/storage/worktrees";

function parseKept(data) {
  if (data === null || typeof data !== "object" || !Array.isArray(data.projects) || !Number.isInteger(data.count) || typeof data.size !== "string") return null;
  const projects = data.projects.filter((p) => p !== null && typeof p === "object" && typeof p.id === "string" && typeof p.name === "string" && Array.isArray(p.worktrees));
  return { count: data.count, size: data.size, projects: projects.map((p) => ({ id: p.id, name: p.name, worktrees: p.worktrees.filter((w) => w !== null && typeof w === "object" && typeof w.branch === "string" && typeof w.version === "string") })) };
}

// A branch as a URL path: each segment encoded, its slashes kept, as the delete route reads it.
function branchPath(branch) {
  return branch.split("/").map(encodeURIComponent).join("/");
}

const plural = (n, one, many) => n + " " + (n === 1 ? one : many);
const keptFiles = (worktree) => (Array.isArray(worktree.files) ? worktree.files.filter((f) => f !== null && typeof f === "object" && typeof f.path === "string") : []);
const moreFiles = (worktree) => (Number.isInteger(worktree.more_files) && worktree.more_files > 0 ? worktree.more_files : 0);

function commitsText(worktree) {
  if (!Number.isInteger(worktree.commits)) return "commits unknown";
  return worktree.commits === 0 ? "merged" : plural(worktree.commits, "commit", "commits") + " not on " + (worktree.target || "the target");
}

function deleteQuestion(worktree) {
  const files = keptFiles(worktree);
  const more = moreFiles(worktree);
  // The paths are on the worktree's card right above the question, so it names only what is lost.
  if (worktree.safe === true) {
    return "Delete this worktree and its branch?" + (files.length > 0 || more > 0 ? " Its ignored files are deleted with it." : "");
  }
  const parts = [];
  if (Number.isInteger(worktree.commits) && worktree.commits > 0) parts.push(commitsText(worktree));
  else if (!Number.isInteger(worktree.commits)) parts.push("commits that could not be counted");
  if (files.some((f) => f.kind !== "ignored") || more > 0) parts.push("its changed files");
  if (worktree.files_unreadable === true) parts.push("files that could not be read");
  return "Delete this worktree and its branch" + (parts.length > 0 ? " with " + parts.join(" and ") : "") + "? They are lost for good.";
}

function KeptWorktree({ projectId, worktree, onDelete }) {
  const files = keptFiles(worktree);
  const more = moreFiles(worktree);
  return (
    <li className="kept-card" data-kept-worktree>
      <div className="kept-card-head">
        <code className="kept-branch">{worktree.branch}</code>
        <span className="muted">{typeof worktree.size === "string" ? worktree.size : "size unknown"}</span>
      </div>
      <div className="kept-facts">
        {typeof worktree.run === "string" && RUN_ID.test(worktree.run) ? (
          <a href={runHref(projectId, worktree.run)} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(runHref(projectId, worktree.run)); } }}>{worktree.run}</a>
        ) : null}
        {typeof worktree.ending === "string" ? <span>{"Run ended: " + worktree.ending}</span> : null}
        <span className={worktree.merged === true ? "kept-merged" : undefined}>{commitsText(worktree)}</span>
      </div>
      {typeof worktree.reason === "string" && worktree.reason !== "" ? <p className="kept-reason muted">{"Kept because " + worktree.reason}</p> : null}
      {files.length > 0 || more > 0 ? (
        <details className="kept-files-details" data-kept-files>
          <summary><span>{files.length === 0 ? plural(more, "file", "files") : plural(files.length, "file", "files") + (more > 0 ? " and " + more + " more" : "")}</span><Icon name="chevronDown" size={16} /></summary>
          <ul className="kept-files">
            {files.map((file) => <li key={file.path} data-kept-file>{file.path + " (" + file.kind + ")"}</li>)}
            {more > 0 ? <li>{"and " + more + " more"}</li> : null}
          </ul>
        </details>
      ) : null}
      {worktree.files_unreadable === true ? <p className="muted">The changed files could not be read.</p> : null}
      <div className="kept-actions">
        <button type="button" className="danger-button" data-kept-delete onClick={onDelete}><Icon name="trash" size={16} />Delete</button>
      </div>
    </li>
  );
}

// One delete-merged takes at most 200 worktrees and 64 KiB; batches stay at half of each.
const MERGED_BATCH = 100;
const MERGED_BATCH_BYTES = 32 * 1024;

function mergedBatches(all) {
  const encoder = new TextEncoder();
  const batches = [];
  let current = [];
  let bytes = 0;
  for (const worktree of all) {
    const size = encoder.encode(JSON.stringify(worktree)).length + 1;
    if (current.length > 0 && (current.length >= MERGED_BATCH || bytes + size > MERGED_BATCH_BYTES)) {
      batches.push(current);
      current = [];
      bytes = 0;
    }
    current.push(worktree);
    bytes += size;
  }
  if (current.length > 0) batches.push(current);
  return batches;
}

function KeptWorktrees({ token, onSignedOut }) {
  const [kept, setKept] = useState(null);
  const [absent, setAbsent] = useState(false);
  const [asking, setAsking] = useState(null);
  const [typed, setTyped] = useState("");
  const [sending, setSending] = useState(false);
  const [lines, setLines] = useState([]);
  const [note, setNote] = useState("");
  const load = async () => {
    try {
      const response = await get(API_WORKTREES, token);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      if (response.status === 404) {
        setAbsent(true);
        return;
      }
      if (!response.ok) {
        setNote(await reasonOf(response, "The kept worktrees could not be read."));
        return;
      }
      const found = parseKept(await response.json());
      if (found === null) setNote("The kept worktrees could not be read.");
      else setKept(found);
    } catch (unreachable) {
      setNote("The server could not be reached.");
    }
  };
  useEffect(() => {
    load();
  }, []);
  const post = async (path, body, done) => {
    setSending(true);
    setNote("");
    setLines([]);
    try {
      const response = await actionRequest(path, token, body);
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
        setLines(done(reply));
      } else {
        setNote(await reasonOf(response, "The worktree could not be deleted."));
      }
    } catch (unreachable) {
      setNote("The server could not be reached.");
    }
    setAsking(null);
    setTyped("");
    setSending(false);
    await load();
  };
  if (absent) return null;
  const safe = kept === null ? [] : kept.projects.flatMap((p) => p.worktrees.filter((w) => w.safe === true).map((w) => ({ project: p.id, branch: w.branch, version: w.version })));
  const ask = (next) => {
    setAsking(next);
    setTyped("");
    setLines([]);
    setNote("");
  };
  const deleteOne = ({ project, worktree }) => {
    const body = worktree.safe === true ? { version: worktree.version } : { version: worktree.version, force: true };
    post(API_WORKTREES + "/" + encodeURIComponent(project) + "/" + branchPath(worktree.branch) + "/delete", body, () => ["Deleted " + worktree.branch + "."]);
  };
  // Sent a batch at a time, as the server takes only so many in one request; a batch it refuses stops the rest.
  const deleteMerged = async (all) => {
    setSending(true);
    setNote("");
    setLines([]);
    let gone = 0;
    const failed = [];
    try {
      for (const batch of mergedBatches(all)) {
        const response = await actionRequest(API_WORKTREES + "/delete-merged", token, { worktrees: batch });
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        if (!response.ok) {
          setNote(await reasonOf(response, "The worktrees could not be deleted."));
          break;
        }
        let reply = null;
        try {
          reply = await response.json();
        } catch (unreadable) {
          reply = null;
        }
        const results = reply !== null && Array.isArray(reply.results) ? reply.results.filter((r) => r !== null && typeof r === "object") : [];
        gone += results.filter((r) => r.deleted === true).length;
        failed.push(...results.filter((r) => r.deleted !== true).map((r) => String(r.branch) + ": " + (typeof r.error === "string" ? r.error : "not deleted")));
      }
    } catch (unreachable) {
      setNote("The server could not be reached.");
    }
    setLines(["Deleted " + gone + " of " + plural(all.length, "merged worktree.", "merged worktrees."), ...failed]);
    setAsking(null);
    setTyped("");
    setSending(false);
    await load();
  };
  const heading = kept === null || kept.count === 0 ? "Kept worktrees" : "Kept worktrees · " + kept.count + " · " + kept.size;
  return (
    <div className="kept-worktrees" data-kept-worktrees>
      <h3 className="settings-subhead">{heading}</h3>
      <p className="muted">Worktrees orq made that a run left behind, across every project.</p>
      {kept !== null && kept.count === 0 ? <p className="muted">No kept worktrees</p> : null}
      {kept !== null
        ? kept.projects.filter((p) => p.worktrees.length > 0).map((project) => (
            <section key={project.id} className="kept-project" aria-label={project.name}>
              <h4>{project.name}</h4>
              <ul className="kept-list">
                {project.worktrees.map((worktree) => (
                  <Fragment key={worktree.branch}>
                    <KeptWorktree projectId={project.id} worktree={worktree} onDelete={() => ask({ project: project.id, worktree })} />
                    {asking !== null && asking.worktree === worktree ? (
                      <li className="kept-confirm">
                        <Confirm question={deleteQuestion(worktree)} answer="Yes, delete" tone="danger" busy={sending} blocked={worktree.safe !== true && typed !== worktree.branch} onConfirm={() => deleteOne(asking)} onCancel={() => setAsking(null)}>
                          {worktree.safe !== true ? (
                            <label>
                              {"Type " + worktree.branch + " to confirm"}
                              <input type="text" autoComplete="off" spellCheck="false" data-kept-branch-typed value={typed} onChange={(event) => setTyped(event.target.value)} />
                            </label>
                          ) : null}
                        </Confirm>
                      </li>
                    ) : null}
                  </Fragment>
                ))}
              </ul>
            </section>
          ))
        : null}
      {safe.length >= 2 && asking === null ? (
        <div className="settings-block settings-actions">
          <button type="button" className="danger-button" data-kept-delete-merged onClick={() => ask({ merged: safe })}><Icon name="trash" size={16} />Delete all merged</button>
        </div>
      ) : null}
      {asking !== null && asking.merged !== undefined ? (
        <div className="settings-block">
          <Confirm question={"Delete " + asking.merged.length + " merged worktrees and their branches?"} answer="Yes, delete all" tone="danger" busy={sending} onConfirm={() => deleteMerged(asking.merged)} onCancel={() => setAsking(null)}>
            <ul className="kept-files">{asking.merged.map((w) => <li key={w.project + "/" + w.branch}>{w.branch}</li>)}</ul>
          </Confirm>
        </div>
      ) : null}
      {lines.length > 0 ? <div className="settings-block"><ul role="status" className="storage-result">{lines.map((line, index) => <li key={index}>{line}</li>)}</ul></div> : null}
      <SettingsNotes sending={sending} sendingText="Deleting…" note={note} conflict={false} />
    </div>
  );
}

function SettingsView({ settings, reloads, token, onSignedOut, onChanged }) {
  const on = settings.remote_actions === true;
  return (
    <>
      <section className="card settings-card">
        <SettingsHead icon="sliders">Run settings</SettingsHead>
        <RemoteActionsSwitch key={String(on) + " " + settings.version} on={on} version={settings.version} token={token} onSignedOut={onSignedOut} onChanged={onChanged} />
      </section>
      <PushCard key={"push " + reloads} token={token} />
      {settings.server !== null ? <ServerCard server={settings.server} on={on} token={token} onSignedOut={onSignedOut} onChanged={onChanged} /> : null}
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
          <KeptWorktrees token={token} onSignedOut={onSignedOut} />
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
  return data && Array.isArray(data.features) && data.features.every(isFeature) ? { rows: data.features, actions: data.remote_actions === true, startBlocked: typeof data.start_blocked === "string" && data.start_blocked !== "" ? data.start_blocked : null, restartPending: data.restart_pending === true } : null;
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
// blocked holds back only the answer, such as until a name is typed back, while Cancel stays open.
function Confirm({ question, detail = null, answer, tone, busy = false, blocked = false, onConfirm, onCancel, children }) {
  return (
    <div className="confirm" role="group" aria-label={question}>
      {children ? <div className="confirm-fields">{children}</div> : null}
      <p className="confirm-question">{question}{detail !== null ? <span className="confirm-detail muted">{detail}</span> : null}</p>
      <div className="confirm-actions">
        <button type="button" className={toneClass(tone === "danger" ? "danger" : "primary")} data-confirm disabled={busy || blocked} onClick={onConfirm}>{answer}</button>
        <button type="button" data-cancel disabled={busy} onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

// The link a start's confirmation carries to the new run's orchestration page, or nothing for an empty or
// unroutable run id.
// A start's confirmation with the server's warning about it, such as the run not seeing commits only the
// local target branch has, when the reply carries one.
function withWarning(text, body) {
  return body !== null && typeof body.warning === "string" && body.warning !== "" ? text + " Warning: " + body.warning + "." : text;
}

function OpenRun({ projectId, run }) {
  if (!RUN_ID.test(run)) return null;
  const href = "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(run);
  return <a data-open-run={run} href={href} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>Open run</a>;
}

// Why a start is held back, beside its disabled button; nothing when it is not.
function BlockedNote({ reason }) {
  return reason === null ? null : <span role="status" className="run-action-note muted" data-start-blocked>{reason}</span>;
}

// The reason a control's project refuses a run start now, or null.
function blockedOf(control) {
  return control !== null && typeof control.startBlocked === "string" && control.startBlocked !== "" ? control.startBlocked : null;
}

// opensIn: the project whose orchestration page the run the action started is linked to, or null for an
// action that starts no run. blocked: why the button is disabled now, shown beside it, or null.
// float: in a run table's row on a wide screen, the question floats in a panel anchored to the button (see
// FloatingQuestion) so the row keeps its size; on a phone it opens in the row as everywhere else.
// With `answers`, its question carries a field for an answer the resumed session is given; a blank one is not sent.
// With a `decision` the run stopped at, the question shows it above the field, and a blank answer asks once more
// before the action is sent.
const REASK_DECISION = "Resume without answering? The session will stop at the same decision.";

function RunAction({ kind, label, question, path, check, token, onSignedOut, onDone, done, tone, icon, asking = false, onCancel = null, opensIn = null, blocked = null, blockedNote = true, float = false, body: sent = {}, answers = false, decision = null }) {
  const [phase, setPhase] = useState(asking && blocked === null ? "asking" : "idle");
  const floating = float && !usePhone();
  const button = useRef(null);
  // A floating question closed by another one opening while its answer is being sent: only hidden, so the
  // action stays sending (its button disabled) until the answer arrives.
  const [hidden, setHidden] = useState(false);
  const [note, setNote] = useState("");
  const [run, setRun] = useState("");
  const [asked, setAsked] = useState(typeof question === "string" ? question : "");
  const [answer, setAnswer] = useState("");
  // Set once a blank answer to a decision was confirmed: the question asks again, and the next confirm sends.
  const [warned, setWarned] = useState(false);
  const field = answers ? (
    <>
      {decision !== null ? <p className="confirm-decision" data-decision>{decision}</p> : null}
      <label className="field">
        <span>{decision !== null ? "Your answer to the decision" : "Answer for the session (optional)"}</span>
        <textarea data-answer rows={decision !== null ? 4 : 2} value={answer} maxLength={8000} onChange={(event) => setAnswer(event.target.value)} />
      </label>
    </>
  ) : null;
  const posed = warned ? REASK_DECISION : asked;
  const confirm = () => {
    if (answers && decision !== null && !warned && answer.trim() === "") {
      setWarned(true);
      return;
    }
    send();
  };
  const ask = async () => {
    setNote("");
    setHidden(false);
    setWarned(false);
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
    const answered = answer.trim() !== "";
    try {
      const response = await actionRequest(path, token, answered ? { ...sent, answer: answer.trim() } : sent);
      if (response.status === 401) {
        onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        // Done for good: the control stays closed while the list is read again, so the same action cannot be sent twice.
        const started = body !== null && typeof body.run === "string" ? body.run : "";
        setNote(done(started, body ?? {}, answered));
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
  const cancel = () => (onCancel === null ? setPhase("idle") : onCancel());
  if (floating) {
    // The button stays in its row while the question is open, so the row keeps its size.
    const open = phase === "asking" || phase === "sending";
    return (
      <div className="run-action">
        {phase !== "done" ? <button type="button" ref={button} className={toneClass(tone)} {...{ ["data-" + kind]: "" }} disabled={blocked !== null || phase === "checking" || phase === "sending"} aria-expanded={open ? "true" : "false"} title={blocked ?? undefined} onClick={open ? cancel : ask}>{icon ? <Icon name={icon} size={16} /> : null}{label}</button> : null}
        {phase === "idle" && note === "" && blockedNote ? <BlockedNote reason={blocked} /> : null}
        {phase === "checking" ? <span role="status" className="sr-only">Checking…</span> : null}
        {phase === "sending" ? <span role="status" className="sr-only">Sending…</span> : null}
        {open && !hidden ? (
          <FloatingQuestion anchor={button} busy={phase === "sending"} onClose={phase === "sending" ? () => setHidden(true) : cancel}>
            <Confirm question={posed} answer={"Yes, " + label.toLowerCase()} tone={tone} busy={phase === "sending"} onConfirm={confirm} onCancel={cancel}>{field}</Confirm>
          </FloatingQuestion>
        ) : null}
        {note !== "" ? <span role="status" className={phase === "done" ? "run-action-note" : "run-action-note error-text"}>{note}</span> : null}
        {phase === "done" && opensIn !== null ? <OpenRun projectId={opensIn} run={run} /> : null}
      </div>
    );
  }
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className={toneClass(tone)} {...{ ["data-" + kind]: "" }} disabled={blocked !== null} title={blocked ?? undefined} onClick={ask}>{icon ? <Icon name={icon} size={16} /> : null}{label}</button> : null}
      {phase === "idle" && note === "" && blockedNote ? <BlockedNote reason={blocked} /> : null}
      {phase === "checking" ? <span role="status">Checking…</span> : null}
      {phase === "asking" ? <Confirm question={posed} answer={"Yes, " + label.toLowerCase()} tone={tone} onConfirm={confirm} onCancel={cancel}>{field}</Confirm> : null}
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

// The steps a frontend run's design review is made of: a run that ended needing review or failed in one of
// them takes a person's own design issues for one more correction round.
const DESIGN_STEPS = ["design_review", "design_fix", "design_code_review"];
const PERSON_SEVERITIES = [["major", "Major"], ["minor", "Minor"], ["critical", "Critical"]];
let personIssueKey = 0;
const blankPersonIssue = () => ({ key: ++personIssueKey, section: "", viewport: "all", expected: "", actual: "", fix: "", severity: "major" });

function takesDesignIssues(run) {
  return (run.state === "needs_review" || run.state === "failed") && DESIGN_STEPS.includes(run.stage) && designOf(run) !== null;
}

// Add design issues on the run page: a form of one or more issues, each a section (the report's section names
// suggested), a viewport (the run's or All), what was expected, what is wrong, how to fix it (optional) and a
// severity. Run another round posts them; the server checks them as `orq run resume --design-issues` does and
// resumes the run with them. A refusal shows its reason in the form, which stays open to be corrected.
function DesignIssuesAction({ run, control }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const [issues, setIssues] = useState(() => [blankPersonIssue()]);
  const design = designOf(run);
  const named = (list, of) => [...new Set(list.filter((item) => item !== null && typeof item === "object" && typeof item[of] === "string" && item[of] !== "").map((item) => item[of]))];
  const viewports = named(design.rounds.flatMap((round) => listOf(round.viewports)), "name").filter((name) => name !== "all");
  const sections = named(design.rounds.flatMap((round) => listOf(round.issues)), "section");
  const listId = "design-issue-sections-" + run.run_id;
  const path = controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/design-issues");
  const change = (key, field, value) => setIssues(issues.map((issue) => (issue.key === key ? { ...issue, [field]: value } : issue)));
  const open = () => {
    setNote("");
    setIssues([blankPersonIssue()]);
    setPhase("asking");
  };
  const send = async () => {
    setPhase("sending");
    setNote("");
    let text = "The issues could not be sent.";
    const sent = issues.map((issue) => ({ section: issue.section.trim(), viewport: issue.viewport, expected: issue.expected.trim(), actual: issue.actual.trim(), ...(issue.fix.trim() === "" ? {} : { fix_direction: issue.fix.trim() }), severity: issue.severity }));
    try {
      const response = await actionRequest(path, control.token, { issues: sent });
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok) {
        const next = body !== null && typeof body.run === "string" && body.run !== "" ? body.run : run.run_id;
        setNote("Correcting your issues as run " + next + ".");
        setPhase("done");
        control.onDone();
        return;
      } else if (response.status === 403) text = "Not allowed.";
      else if (body !== null && typeof body.error === "string") text = body.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("asking");
  };
  const sending = phase === "sending";
  const area = (issue, field, data, label) => (
    <label className="field">
      <span className="field-label">{label}</span>
      <textarea {...{ [data]: "" }} rows={2} maxLength={2000} value={issue[field]} onChange={(event) => change(issue.key, field, event.target.value)} />
    </label>
  );
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" data-design-issues="" onClick={open}><Icon name="plus" size={16} />Add design issues</button> : null}
      {phase === "asking" || sending ? (
        <div className="confirm design-issues-form" role="group" aria-label="Your design issues" data-design-issues-form="">
          <datalist id={listId} data-issue-sections="">{sections.map((section) => <option key={section} value={section} />)}</datalist>
          {issues.map((issue, at) => (
            <fieldset key={issue.key} className="confirm-fields person-issue" data-person-issue={at}>
              <legend>{"Issue " + (at + 1)}</legend>
              <label className="field">
                <span className="field-label">Section</span>
                <input type="text" data-issue-section="" list={listId} maxLength={200} value={issue.section} onChange={(event) => change(issue.key, "section", event.target.value)} />
              </label>
              <label className="field">
                <span className="field-label">Viewport</span>
                <select data-issue-viewport="" value={issue.viewport} onChange={(event) => change(issue.key, "viewport", event.target.value)}>
                  <option value="all">All</option>
                  {viewports.map((name) => <option key={name} value={name}>{name}</option>)}
                </select>
              </label>
              {area(issue, "expected", "data-issue-expected", "Expected")}
              {area(issue, "actual", "data-issue-actual", "What's wrong")}
              {area(issue, "fix", "data-issue-fix", "How to fix (optional)")}
              <label className="field">
                <span className="field-label">Severity</span>
                <select data-issue-severity="" value={issue.severity} onChange={(event) => change(issue.key, "severity", event.target.value)}>
                  {PERSON_SEVERITIES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
                </select>
              </label>
              {issues.length > 1 ? <button type="button" data-design-issue-remove="" disabled={sending} onClick={() => setIssues(issues.filter((other) => other.key !== issue.key))}>Remove</button> : null}
            </fieldset>
          ))}
          <div className="confirm-actions">
            <button type="button" data-design-issue-add="" disabled={sending} onClick={() => setIssues([...issues, blankPersonIssue()])}>Add another</button>
            <button type="button" className="primary-button" data-design-issues-send="" disabled={sending} onClick={send}>Run another round</button>
            <button type="button" data-cancel="" disabled={sending} onClick={() => setPhase("idle")}>Cancel</button>
          </div>
          {sending ? <span role="status">Sending…</span> : null}
          {note !== "" ? <span role="status" className="run-action-note error-text" data-design-issues-refused="">{note}</span> : null}
        </div>
      ) : null}
      {phase === "done" ? <span role="status" className="run-action-note">{note}</span> : null}
    </div>
  );
}

// Project default: a start that names no profiles, so the run uses the project's own agents.
const PROJECT_DEFAULT = "";

// The launch picker's two choices, the implementer and the reviewer, each from the offered profiles, and
// Project default when the server offers it. Each is a labelled field, as in the feature editor.
function LaunchPickers({ profiles, projectDefault, implementer, reviewer, onImplementer, onReviewer }) {
  const options = [
    ...(projectDefault ? [<option key={PROJECT_DEFAULT} value={PROJECT_DEFAULT}>Project default</option>] : []),
    ...profiles.map((name) => <option key={name} value={name}>{name}</option>),
  ];
  return (
    <>
      <label className="field">
        <span className="field-label">Implementer</span>
        <select data-launch-implementer="" value={implementer} onChange={(event) => onImplementer(event.target.value)}>{options}</select>
      </label>
      <label className="field">
        <span className="field-label">Reviewer</span>
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
  const blocked = blockedOf(control);
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
        setNote(withWarning(run === "" ? "Started." : "Started run " + run + ".", body));
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
      {phase === "idle" ? <button type="button" className="secondary-button" data-start-with="" disabled={blocked !== null} title={blocked ?? undefined} onClick={ask}><Icon name="sliders" size={16} />Start with…</button> : null}
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

// A run's feature as a link to its page: a plain left click opens it in the app, a modified one is left to the browser.
function FeatureLink({ projectId, name, className = undefined, children }) {
  const href = featureHref(projectId, name);
  return <a className={className} data-run-feature href={href} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>{children}</a>;
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

// The app's project picker: a button showing the chosen project (or the placeholder, muted) with a chevron, that
// opens a listbox of `choices` ({ id, name }); the chosen one is aria-selected. `buttonProps` adds the button's own
// data attributes; its data-value is the chosen id. Enter, Space or Down opens the list, Up and Down move, Enter
// picks; Escape (taken before a popup's own Escape, which then leaves it be) and a press outside close the list.
function ProjectChooser({ choices, value, onChoose, label = "Project", placeholder = "", disabled = false, onOpen = null, buttonProps = {} }) {
  const [open, setOpen] = useState(false);
  // The option the keyboard is on, -1 when none.
  const [active, setActive] = useState(-1);
  const box = useRef(null);
  const button = useRef(null);
  const list = useRef(null);
  const close = () => {
    setOpen(false);
    setActive(-1);
  };
  useEffect(() => {
    if (!open) return undefined;
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      close();
      if (box.current && box.current.contains(document.activeElement) && button.current) button.current.focus();
    };
    const onPress = (event) => {
      if (box.current && !box.current.contains(event.target)) close();
    };
    document.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("pointerdown", onPress);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("pointerdown", onPress);
    };
  }, [open]);
  useEffect(() => {
    if (!open || active < 0 || list.current === null) return;
    const option = list.current.querySelectorAll("[role=option]")[active];
    if (option) option.focus();
  }, [open, active]);
  const chosen = choices.findIndex((choice) => choice.id === value);
  const current = chosen < 0 ? null : choices[chosen];
  const show = () => {
    if (onOpen !== null) onOpen();
    setOpen(true);
  };
  const choose = (choice) => {
    close();
    onChoose(choice.id);
    if (button.current) button.current.focus();
  };
  const onKeyDown = (event) => {
    const last = choices.length - 1;
    if (!open) {
      if (event.target === button.current && (event.key === "Enter" || event.key === " " || event.key === "ArrowDown")) {
        event.preventDefault();
        show();
        setActive(choices.length === 0 ? -1 : Math.max(chosen, 0));
      }
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (choices.length === 0) return;
      const from = active < 0 ? chosen : active;
      setActive(event.key === "ArrowDown" ? Math.min(from + 1, last) : Math.max(from < 0 ? last : from - 1, 0));
    } else if ((event.key === "Enter" || event.key === " ") && choices[active] !== undefined) {
      event.preventDefault();
      choose(choices[active]);
    } else if (event.key === "Tab") {
      close();
    }
  };
  // Space clicks a button when it is let go; the keydown above already acted on it.
  const onKeyUp = (event) => {
    if (event.key === " ") event.preventDefault();
  };
  return (
    <div className="project-chooser" ref={box} onKeyDown={onKeyDown} onKeyUp={onKeyUp}>
      <button type="button" ref={button} className="project-switcher" {...buttonProps} data-value={value ?? ""} aria-label={label} aria-haspopup="listbox" aria-expanded={open ? "true" : "false"} disabled={disabled} onClick={() => { if (open) close(); else show(); }}>
        <span className={current === null ? "project-switcher-text muted" : "project-switcher-text"}>{current === null ? placeholder : current.name}</span>
        <Icon name="chevronDown" size={18} />
      </button>
      <ul className="project-list" ref={list} role="listbox" aria-label={label} hidden={!open}>
        {choices.map((choice) => (
          <li key={choice.id ?? ""} role="presentation">
            <button type="button" role="option" tabIndex={-1} data-project-choice={choice.id ?? ""} aria-selected={choice.id === value ? "true" : "false"} onClick={() => choose(choice)}>{choice.name}</button>
          </li>
        ))}
      </ul>
    </div>
  );
}

// The project chooser at the top of the page; choosing one opens that project's same section.
function ProjectSelect({ projects, projectId, section = "features", onOpen = null }) {
  if ((projects === null || projects.length === 0) && onOpen === null) return null;
  const known = projectId === null || projects.some((project) => project.id === projectId);
  const choices = [{ id: null, name: "All projects" }, ...(known ? [] : [{ id: projectId, name: projectId }]), ...projects];
  const open = (id) => navigate(id === null ? "/" + section : "/projects/" + encodeURIComponent(id) + "/" + section);
  return (
    <div className="project-picker">
      <span className="project-switch-prefix">Project:</span>
      <ProjectChooser choices={choices} value={projectId} onChoose={open} onOpen={onOpen} buttonProps={{ "data-project-select": true }} />
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


// A feature's state in words ("In progress", not "in_progress"); a state the page does not know is
// itself in sentence case, neutral.
function FeatureState({ status }) {
  const known = FEATURE_GROUPS.includes(status);
  return <StatusPill kind={known ? status : String(status ?? "")} tone={known ? undefined : "neutral"} icon={known ? undefined : "flag"} data-feature-state={known ? status : "other"} />;
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
      {phase === "idle" ? <button type="button" className="secondary-button" data-promote-marked onClick={() => setPhase("asking")}><Icon name="arrowUp" size={16} />{label}</button> : null}
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

// The popup's launch marked: "Start N" reads the launch profiles and asks with the picked pair. One marked
// feature starts a plain run of it; two or more start one group run, sent as one start at the first feature
// with the whole group in the order given, and the one run started or refused is reported.
function StartMarked({ names, control, onFinished, place = null }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState("");
  const choice = useLaunchChoice();
  const label = "Start " + names.length + placeSuffix(place);
  const grouped = names.length > 1;
  const blocked = blockedOf(control);
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
    const name = names.join(", ");
    let text = name + ": the run could not be started.";
    let ok = false;
    let started = "";
    try {
      const body = grouped ? { group: names, ...choice.body } : choice.body;
      const response = await actionRequest(controlPath(control, "features/" + encodeURIComponent(names[0]) + "/runs"), control.token, body);
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const reply = await jsonOrNull(response);
      ok = response.ok;
      const run = reply !== null && typeof reply.run === "string" ? reply.run : "";
      if (ok) {
        const what = grouped ? "group run" : "run";
        text = withWarning(run === "" ? "Started " + name + "." : "Started " + what + " " + run + " for " + name + ".", reply);
        started = run;
      }
      else if (response.status === 403) text = name + ": Not allowed.";
      else if (reply !== null && typeof reply.error === "string") text = name + ": " + reply.error;
    } catch (unreachable) {
      text = name + ": The server could not be reached.";
    }
    setPhase("idle");
    onFinished([{ name, ok, text, run: started, projectId: control.projectId }]);
  };
  const asked = "Start " + featuresPhrase(names, place) + (grouped ? " as one group run" : "") + " with " + choice.phrase + "?";
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="primary-button" data-start-marked disabled={blocked !== null} title={blocked ?? undefined} onClick={ask}><Icon name="play" size={16} />{label}</button> : null}
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

// Sends features to the project's run queue: the server starts each in turn when its limits allow, and
// never on its own approval. One request for all of them, so the server audits them as one change.
async function queueFeatures(control, names) {
  try {
    const response = await actionRequest(controlPath(control, "queue"), control.token, { features: names });
    if (response.status === 401) return { signedOut: true };
    const reply = await jsonOrNull(response);
    if (response.ok) return { ok: true, text: "Queued " + names.join(", ") + "." };
    if (response.status === 403) return { ok: false, text: "Not allowed." };
    return { ok: false, text: reply !== null && typeof reply.error === "string" ? reply.error : "They could not be queued." };
  } catch (unreachable) {
    return { ok: false, text: "The server could not be reached." };
  }
}

// "Queue" on a pending feature's page: asks, then adds it to the end of the project's queue.
function QueueAction({ feature, control }) {
  const [phase, setPhase] = useState("idle");
  const [note, setNote] = useState(null);
  const send = async () => {
    setPhase("sending");
    const result = await queueFeatures(control, [feature.name]);
    if (result.signedOut) {
      control.onSignedOut();
      return;
    }
    setNote(result);
    setPhase("idle");
  };
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="secondary-button" data-queue-feature onClick={() => { setNote(null); setPhase("asking"); }}><Icon name="clock" size={16} />Queue</button> : null}
      {phase === "asking" ? (
        <Confirm question={"Queue " + feature.name + "?"} detail="The server starts it when the project's limits and queue hours allow." answer="Yes, queue" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")} />
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
      {note !== null ? <span role="status" className={note.ok ? "run-action-note" : "run-action-note error-text"}>{note.text}</span> : null}
    </div>
  );
}

// "Queue N" for the marked pending features: asks once, then queues them in dependency order.
function QueueMarked({ names, control, onFinished, place = null }) {
  const [phase, setPhase] = useState("idle");
  const send = async () => {
    setPhase("sending");
    const result = await queueFeatures(control, names);
    if (result.signedOut) {
      control.onSignedOut();
      return;
    }
    setPhase("idle");
    onFinished([{ name: names.join(", "), ok: result.ok, text: result.text }]);
  };
  return (
    <div className="run-action">
      {phase === "idle" ? <button type="button" className="secondary-button" data-queue-marked onClick={() => setPhase("asking")}><Icon name="clock" size={16} />{"Queue " + names.length + placeSuffix(place)}</button> : null}
      {phase === "asking" ? (
        <Confirm question={"Queue " + featuresPhrase(names, place) + "?"} detail="The server starts each when the project's limits and queue hours allow." answer="Yes, queue" tone="primary" onConfirm={send} onCancel={() => setPhase("idle")} />
      ) : null}
      {phase === "sending" ? <span role="status">Sending…</span> : null}
    </div>
  );
}

// The states a queue entry can be in (SPEC section 7); each wears the same status pill as a run in that
// state, with its own icon, so Next differs from Queued by more than colour. "No longer needed" is neutral.
const QUEUE_STATES = ["running", "paused", "review", "failed", "next", "queued", "waiting", "skipped"];

// A queue entry's state as a status pill that always shows its words. A state the page does not know
// shows as itself, neutral.
function QueueStateBadge({ state }) {
  const known = QUEUE_STATES.includes(state);
  return <StatusPill kind={known ? state : undefined} label={known ? undefined : String(state)} className="queue-state" data-queue-state={state} />;
}

// What a queue entry shows: a feature's start by its name, a queued Resume, Review again or Accept by its label.
function queueName(entry) {
  return typeof entry.label === "string" && entry.label !== "" && entry.kind !== "start" && entry.kind !== undefined ? entry.label : entry.feature;
}

function isQueue(queue) {
  const text = (value) => value === null || typeof value === "string";
  const wait = queue === null || typeof queue !== "object" ? null : queue.waiting_for_review ?? null;
  return queue !== null && typeof queue === "object" && text(queue.paused) && Array.isArray(queue.entries)
    && (wait === null || (typeof wait === "object" && typeof wait.feature === "string" && text(wait.mr_url ?? null)))
    && queue.entries.every((entry) => entry !== null && typeof entry === "object" && typeof entry.feature === "string" && typeof entry.state === "string" && entry.state !== "" && text(entry.run ?? null) && text(entry.reason ?? null) && text(entry.mr_url ?? null));
}

// "Waiting for COM-01's MR to merge" (or its review, with no MR) while the queue starts nothing more
// until a run it started is reviewed; null when it does not wait.
function QueueReviewWait({ queue }) {
  const wait = queue.waiting_for_review ?? null;
  if (wait === null) return null;
  return <p className="queue-waiting" data-queue-waiting-review><Icon name="clock" size={16} />{"Waiting for " + (typeof wait.label === "string" && !wait.label.startsWith("Start ") ? wait.label : wait.feature) + (typeof wait.mr_url === "string" ? "'s MR to merge" : "'s review")}</p>;
}

// What one queue entry's row holds, on a project's queue and on the all-projects one: its marker on the
// timeline with its position, toned by its state; then its name, on the next line its state badge with
// its MR and run links, and its reason under them; and, when given, the controls that reorder and remove it.
// `rail` is the part of the rail down to the next entry (see queueRails), or null for the last entry.
function QueueEntryRow({ entry, position, projectId, controls, rail }) {
  return (
    <>
      <span className="queue-marker" data-queue-marker={entry.state} aria-hidden="true">{position}</span>
      {rail !== null ? <span className="queue-rail" data-queue-rail={rail} aria-hidden="true" /> : null}
      <div className="queue-entry-text">
        <span className="queue-feature">{queueName(entry)}</span>
        <div className="queue-entry-meta">
          <QueueStateBadge state={entry.state} />
          <QueueMr entry={entry} />
          {typeof entry.run === "string" ? <OpenRun projectId={projectId} run={entry.run} /> : null}
        </div>
        {typeof entry.reason === "string" && entry.reason !== "" ? <p className="queue-reason">{entry.reason}</p> : null}
      </div>
      {controls}
    </>
  );
}

// The rail part below each entry, in order: "passed" from the top down to the last entry whose run started
// (running or paused), so the rail shows where the queue is, "ahead" after it, and null below the last
// entry, where the rail stops.
function queueRails(entries) {
  const running = entries.map((entry) => entry.state === "running" || entry.state === "paused").lastIndexOf(true);
  return entries.map((entry, index) => (index === entries.length - 1 ? null : index < running ? "passed" : "ahead"));
}

// A pause reason that names its run ("the run <run> ..."): the run id and what comes after it, or null.
const PAUSED_BY_RUN = /^the run (\S+)([\s\S]*)$/;

// A paused queue's banner at the top of its card, in the waiting colours: the pause icon, "Paused" and
// why, with the run it names linked to that run's page, and whatever action (Resume) is given beside it.
function QueuePausedBanner({ reason, projectId, children = null }) {
  const named = typeof projectId === "string" ? PAUSED_BY_RUN.exec(reason) : null;
  const run = named !== null && RUN_ID.test(named[1]) ? named[1] : null;
  const href = run === null ? null : runHref(projectId, run);
  return (
    <div className="banner waiting-banner queue-paused" data-queue-paused>
      <Icon name="pause" size={16} />
      {href === null ? (
        <p className="queue-paused-text">{"Paused: " + reason}</p>
      ) : (
        <p className="queue-paused-text">
          {"Paused: the run "}
          <a href={href} data-queue-paused-run={run} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>{run}</a>
          {named[2]}
        </p>
      )}
      {children}
    </div>
  );
}

// The link to a queued run's MR, when it has one an https address points to.
function QueueMr({ entry }) {
  const href = typeof entry.mr_url === "string" ? safeHttpsLink(entry.mr_url) : null;
  if (href === null) return null;
  return (
    <a className="queue-mr" data-queue-mr href={href} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">
      MR
      <Icon name="external" size={16} />
    </a>
  );
}

function parseQueue(data) {
  return data && isQueue(data.queue) ? { queue: data.queue, actions: data.remote_actions === true } : null;
}

// The names in `order` with the one at `from` moved to `to`.
function moved(order, from, to) {
  const next = [...order];
  const [name] = next.splice(from, 1);
  next.splice(to, 0, name);
  return next;
}

// A project's run queue in the order it starts: each entry's state, why it waits, and its run. With
// remote actions on it pauses and resumes, removes after asking, and reorders by drag or by ↑/↓; each
// change shows the queue the server answers with.
function QueueView({ data, control, projectId, push = null }) {
  const [queue, setQueue] = useState(data.queue);
  const [phase, setPhase] = useState("idle");
  const [removing, setRemoving] = useState(null);
  const [note, setNote] = useState("");
  const [dragged, setDragged] = useState(null);
  const [over, setOver] = useState(null);
  const busy = phase === "sending";
  const endDrag = () => { setDragged(null); setOver(null); };
  const change = async (tail, body) => {
    setPhase("sending");
    setNote("");
    let text = "The queue could not be changed.";
    try {
      const response = await actionRequest(controlPath(control, "queue/" + tail), control.token, body);
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const reply = await jsonOrNull(response);
      if (response.ok) {
        if (reply !== null && isQueue(reply.queue)) setQueue(reply.queue);
        setPhase("idle");
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (reply !== null && typeof reply.error === "string") text = reply.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  // Pulls the project's target again after a merge left it behind; the warning is what the pull answers.
  const pull = async () => {
    setPhase("sending");
    setNote("");
    let text = "The target could not be pulled.";
    try {
      const response = await actionRequest(controlPath(control, "pull"), control.token, {});
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const reply = await jsonOrNull(response);
      if (response.ok && reply !== null) {
        const warning = typeof reply.target_pull_warning === "string" ? reply.target_pull_warning : null;
        setQueue({ ...queue, target_pull_warning: warning });
        setNote(warning === null && typeof reply.message === "string" ? reply.message : "");
        setPhase("idle");
        return;
      }
      if (response.status === 403) text = "Not allowed.";
      else if (reply !== null && typeof reply.error === "string") text = reply.error;
    } catch (unreachable) {
      text = "The server could not be reached.";
    }
    setNote(text);
    setPhase("idle");
  };
  const pullWarning = typeof queue.target_pull_warning === "string" ? queue.target_pull_warning : null;
  const names = queue.entries.map((entry) => entry.feature);
  const rails = queueRails(queue.entries);
  const reorder = (from, to) => {
    if (to < 0 || to >= names.length || to === from) return;
    change("reorder", { order: moved(names, from, to) });
  };
  return (
    <section className="card queue-section" data-queue-section>
      {pullWarning !== null ? (
        <div className="queue-pull-warning" role="note" data-target-pull-warning>
          <p className="error-text"><Icon name="alert" size={16} />{pullWarning}</p>
          {control !== null ? <button type="button" data-target-pull-retry disabled={busy} onClick={pull}><Icon name="rotate" size={16} />Retry</button> : null}
        </div>
      ) : null}
      <Unpushed unpushed={queue.unpushed ?? null} subjects push={push} />
      {queue.paused !== null ? (
        <QueuePausedBanner reason={queue.paused} projectId={projectId}>
          {control !== null ? <button type="button" className="primary-button" data-queue-resume disabled={busy} onClick={() => change("resume", {})}><Icon name="play" size={16} />Resume</button> : null}
        </QueuePausedBanner>
      ) : (
        <div className="queue-head">
          {(queue.waiting_for_review ?? null) !== null ? <QueueReviewWait queue={queue} /> : <p className="muted">{queue.entries.length === 0 ? "Nothing is queued." : "The server starts the next feature when the project's limits and queue hours allow."}</p>}
          {control !== null ? <button type="button" data-queue-pause disabled={busy} onClick={() => change("pause", {})}><Icon name="pause" size={16} />Pause</button> : null}
        </div>
      )}
      {queue.entries.length > 0 ? (
        <ol className="queue-list">
          {queue.entries.map((entry, index) => {
            const dropping = dragged !== null && over === index && over !== dragged;
            return (
              <li key={entry.feature} className={"queue-entry" + (dragged === index ? " dragging" : "") + (dropping ? " drop-target" : "")} data-queue-entry={entry.feature} draggable={control !== null && !busy}
                onDragStart={(event) => { setDragged(index); if (event.dataTransfer) event.dataTransfer.effectAllowed = "move"; }}
                onDragOver={(event) => { if (dragged !== null) { event.preventDefault(); if (over !== index) setOver(index); } }}
                onDrop={(event) => { event.preventDefault(); if (dragged !== null) reorder(dragged, index); endDrag(); }}
                onDragEnd={endDrag}>
                <QueueEntryRow entry={entry} position={index + 1} projectId={projectId} rail={rails[index]} controls={control !== null ? (
                  <span className="queue-controls">
                    <button type="button" className="icon-button" data-queue-up aria-label={"Move " + queueName(entry) + " up"} disabled={busy || index === 0} onClick={() => reorder(index, index - 1)}><Icon name="arrowUp" size={16} /></button>
                    <button type="button" className="icon-button" data-queue-down aria-label={"Move " + queueName(entry) + " down"} disabled={busy || index === names.length - 1} onClick={() => reorder(index, index + 1)}><Icon name="chevronDown" size={16} /></button>
                    <button type="button" className="icon-button" data-queue-remove aria-label={"Remove " + queueName(entry)} disabled={busy} onClick={() => { setNote(""); setRemoving(entry); }}><Icon name="trash" size={16} /></button>
                  </span>
                ) : null} />
                {/* Dragged down, the entry lands after the one it is over; dragged up, before it. */}
                {dropping ? <span className="queue-drop" data-queue-drop={index < dragged ? "before" : "after"} aria-hidden="true" /> : null}
              </li>
            );
          })}
        </ol>
      ) : null}
      {removing !== null ? (
        <Confirm question={"Remove " + queueName(removing) + " from the queue?"} detail="A run it already started goes on." answer="Yes, remove" tone="danger" onConfirm={() => { const name = removing.feature; setRemoving(null); change("remove", { feature: name }); }} onCancel={() => setRemoving(null)} />
      ) : null}
      {busy ? <span role="status">Sending…</span> : null}
      {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
    </section>
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
// The ledger's rule for a feature name, as orq_features::feature_name_problem says it: letters, digits, '-', '_'
// and '.', not starting with '.'. The first refused character is named with its position (in characters from 1);
// a space or an invisible character only by its code point, so a hidden one can be found.
const SPACING = "\\u0009-\\u000d\\u0020\\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000";
const WHITE_SPACE = new RegExp("^[" + SPACING + "]$", "u");

// The name Add feature saves for what was typed: spaces at the ends dropped, and each run of spaces inside
// (tabs and non-breaking spaces too), with any hyphens beside it, one '-', so "a - b" is one word break.
// Letter case and hyphens typed together are kept. One pass, so a long paste costs no more than its length.
function nameFromTyped(typed) {
  let out = "";
  let run = [];
  const close = (first, last) => {
    // At the start or the end, the spaces on the outer side are dropped first.
    let from = 0;
    let to = run.length;
    if (first) while (from < to && run[from] !== "-") from += 1;
    if (last) while (to > from && run[to - 1] !== "-") to -= 1;
    const kept = run.slice(from, to);
    out += kept.every((c) => c === "-") ? kept.join("") : "-";
    run = [];
  };
  for (const c of typed) {
    if (c === "-" || WHITE_SPACE.test(c)) {
      run.push(c);
    } else {
      close(out === "", false);
      out += c;
    }
  }
  close(out === "", true);
  return out;
}
const INVISIBLE = /^[\u0000-\u001f\u007f-\u009f\u00ad\u061c\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u206f\ufeff\ufff9-\ufffb]$/u;
const DASHES = ["\u2014", "\u2013", "\u2010", "\u2011", "\u2212"];

function featureNameProblem(name) {
  if (name === "") return "a feature name is required";
  if (name.startsWith(".")) return "a feature name may not start with '.'";
  const chars = Array.from(name);
  const index = chars.findIndex((c) => !/^[A-Za-z0-9._-]$/.test(c));
  if (index < 0) return null;
  const bad = chars[index];
  const code = "U+" + bad.codePointAt(0).toString(16).toUpperCase().padStart(4, "0");
  const spacing = WHITE_SPACE.test(bad);
  const shown = bad === " " ? "a space" : spacing || INVISIBLE.test(bad) ? code : '"' + bad + '" (' + code + ")";
  const hint = DASHES.includes(bad) ? " (use '-')" : spacing ? " (use '-' between words)" : "";
  return shown + " at position " + (index + 1) + " is not allowed in a feature name; use letters, digits, '-', '_' or '.'" + hint;
}

// What keeps a new feature's name from being added: the name rule, or a feature already called that.
function addedNameProblem(name, features) {
  const problem = featureNameProblem(name);
  if (problem !== null) return problem;
  return features.some((feature) => feature.name === name) ? '"' + name + '" is already a feature name in this project; choose another' : null;
}

function featureFields(draft, features, empty = null) {
  const blank = (value) => (value.trim() === "" ? empty : value);
  return {
    // Spaces become hyphens (see nameFromTyped); nothing else is rewritten, so a name the rule refuses is refused as typed.
    name: nameFromTyped(draft.name),
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

// Embedded (in the Dashboard's Add feature popup) it adds only, opens at once from `initial`, and leaves the
// popup to its host: Cancel calls onClose, a save calls onFinished(doneText), and `target` (a path and the body
// for the fields) sends it somewhere other than the ledger, such as a Todo item's move. `blocked` holds Save back
// with its reason; `extra` sits above the buttons.
// The description keeps its line breaks. The box grows with the text up to a limit, then scrolls; Preview
// shows the same text as Markdown. The text itself is never changed by either view.
const DESCRIPTION_MIN_ROWS = 3;
const DESCRIPTION_MAX_ROWS = 12;

function DescriptionField({ value, onChange }) {
  const [preview, setPreview] = useState(false);
  const rows = Math.min(DESCRIPTION_MAX_ROWS, Math.max(DESCRIPTION_MIN_ROWS, String(value).split("\n").length));
  return (
    <div className="field field-wide description-field">
      <div className="description-head">
        <label htmlFor="feature-description" className="field-label">Description</label>
        <span className="description-switch" role="group" aria-label="Description view">
          <button type="button" className="description-tab" data-description-write aria-pressed={!preview} onClick={() => setPreview(false)}>Write</button>
          <button type="button" className="description-tab" data-description-preview aria-pressed={preview} onClick={() => setPreview(true)}>Preview</button>
        </span>
      </div>
      {preview ? (
        <div className="description-rendered" data-description-rendered>{String(value).trim() === "" ? <p className="muted">Nothing to preview.</p> : <Markdown text={String(value)} />}</div>
      ) : (
        <textarea id="feature-description" className="description-input" data-field-description rows={rows} value={value} onChange={onChange} />
      )}
    </div>
  );
}

function FeatureEditor({ name = null, features, control, embedded = false, initial = null, target = null, doneText = null, onFinished = null, onClose = null, onSending = null, blocked = null, extra = null }) {
  const adding = name === null;
  const [phase, setPhase] = useState(embedded ? "editing" : "idle");
  const [draft, setDraft] = useState(embedded ? featureDraft(initial === null ? {} : initial) : null);
  const [opened, setOpened] = useState(null);
  const [version, setVersion] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  // Save pressed with an empty name: only then does the empty box say a name is required.
  const [pressed, setPressed] = useState(false);
  // Adding opens in a popup (on a phone the stylesheet lays it in the page); Close or Escape shuts it unsent.
  const popup = adding && !embedded && (phase === "editing" || phase === "asking" || phase === "sending");
  const close = () => {
    setNote("");
    setConflict(false);
    setPhase("idle");
    if (embedded) onClose();
  };
  useEffect(() => {
    if (!popup || phase === "sending") return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") close();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [popup, phase]);
  // An embedded editor tells its popup while a save is out, so the popup holds still until it settles.
  useEffect(() => {
    if (onSending !== null) onSending(phase === "sending");
  }, [phase]);
  const open = async () => {
    setNote("");
    setConflict(false);
    if (adding) {
      setPressed(false);
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
  const nameProblem = adding && draft !== null && (draft.name !== "" || pressed) ? addedNameProblem(fields.name, features) : null;
  // Said when spaces were turned into hyphens, so the name shown in the question is no surprise.
  const savedAs = adding && draft !== null && nameProblem === null && fields.name !== draft.name ? "Saved as " + fields.name : null;
  const send = async () => {
    setPhase("sending");
    let text = "The feature could not be saved.";
    let changed = false;
    try {
      const path = target !== null ? target.path : controlPath(control, adding ? "features" : "features/" + encodeURIComponent(name));
      const response = await actionRequest(path, control.token, target !== null ? target.body(fields) : adding ? { fields } : { fields: changedFields(draft, opened, features), version });
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const body = await jsonOrNull(response);
      if (response.ok && embedded) {
        onFinished(doneText);
        return;
      }
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
          {adding ? (
            <label className="field">
              <span className="field-label">Name</span>
              <input type="text" data-field-name value={draft.name} autoCapitalize="none" autoCorrect="off" spellCheck={false} autoComplete="off" inputMode="text" aria-invalid={nameProblem !== null} aria-describedby={adding ? "feature-name-problem feature-name-saved-as" : "feature-name-problem"} onChange={set("name")} />
              <span id="feature-name-problem" className="field-hint error-text" data-name-problem={nameProblem === null ? undefined : ""} aria-live="polite">{nameProblem}</span>
              {adding ? <span id="feature-name-saved-as" className="field-hint muted" data-saved-as={savedAs === null ? undefined : ""} aria-live="polite">{savedAs}</span> : null}
            </label>
          ) : null}
          <DescriptionField value={draft.description} onChange={set("description")} />
          <label className="field"><span className="field-label">Test file</span><input type="text" data-field-test-file value={draft.test_file} autoComplete="off" spellCheck={false} onChange={set("test_file")} /></label>
          <label className="field"><span className="field-label">Target files</span><input type="text" data-field-target-files value={draft.target_files} autoComplete="off" spellCheck={false} placeholder="src/a.rs, src/b.rs" onChange={set("target_files")} /></label>
          <label className="field"><span className="field-label">Parallelizable</span><select data-field-parallelizable value={draft.parallelizable} onChange={set("parallelizable")}><option value="">Not set</option><option value="yes">Yes</option><option value="no">No</option></select></label>
          {others.length > 0 ? <DependsPicker others={others} chosen={draft.depends_on} onToggle={toggle} /> : null}
          {extra}
          {blocked !== null ? <p className="field-hint muted" data-add-blocked>{blocked}</p> : null}
          <div className="feature-editor-actions">
            <button type="button" className="primary-button" data-feature-save disabled={nameProblem !== null || blocked !== null} onClick={() => { setNote(""); setConflict(false); if (adding && addedNameProblem(fields.name, features) !== null) { setPressed(true); return; } setPhase("asking"); }}><Icon name="check" size={16} />{adding ? "Add" : "Save"}</button>
            <button type="button" data-cancel onClick={close}>Cancel</button>
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
      {phase === "idle" ? (adding ? <button type="button" className="primary-button" data-feature-add onClick={open}><Icon name="plus" size={16} />Add feature</button> : <button type="button" className="secondary-button" data-feature-edit onClick={open}><Icon name="pencil" size={16} />Edit</button>) : null}
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
// A start while the server waits to restart still goes ahead, but the restart then waits for that run too.
const RESTART_START_QUESTION = "orq will restart after this run; start anyway?";

// blockedNote: whether Start shows why it is held back beside it; a feature's page shows that once under its actions instead.
function FeatureAction({ feature, control, asking = false, onCancel = null, blockedNote = true }) {
  const name = encodeURIComponent(feature.name);
  const shared = { token: control.token, onSignedOut: control.onSignedOut, onDone: control.onDone, tone: "primary", asking, onCancel, blockedNote };
  return feature.status === "pending"
    ? <RunAction {...shared} kind="start" label="Start" icon="play" opensIn={control.projectId} blocked={blockedOf(control)} question={control.restartPending === true ? RESTART_START_QUESTION : "Start " + feature.name + "?"} path={controlPath(control, "features/" + name + "/runs")} done={(run, body) => withWarning(run === "" ? "Started." : "Started run " + run + ".", body)} />
    : <RunAction {...shared} kind="promote" label="Promote" icon="arrowUp" question={"Promote " + feature.name + "?"} path={controlPath(control, "features/" + name + "/promote")} done={(_, reply) => "Promoted " + (typeof reply.feature === "string" ? reply.feature : feature.name) + "; it is pending."} />;
}

const LONG_PRESS_MS = 500;
const LONG_PRESS_SLOP = 10;

// A feature is frontend or standard; a missing or unknown kind is standard.
function kindOf(kind) {
  return kind === "frontend" ? "frontend" : "standard";
}

// The kind filter's choice, kept in the URL as ?kind=frontend|standard so a link or a reload keeps it; All is no
// kind in the URL, and anything else there reads as All.
function useKindFilter() {
  const [kind, setKind] = useState(() => {
    const asked = new URLSearchParams(window.location.search).get("kind");
    return asked === "frontend" || asked === "standard" ? asked : "all";
  });
  const choose = (next) => {
    setKind(next);
    const params = new URLSearchParams(window.location.search);
    if (next === "all") params.delete("kind");
    else params.set("kind", next);
    const search = params.toString();
    window.history.replaceState(window.history.state, "", window.location.pathname + (search === "" ? "" : "?" + search) + window.location.hash);
  };
  return [kind, choose];
}

function matchesKind(filter, kind) {
  return filter === "all" || kindOf(kind) === filter;
}

// The kind filter: one select, on a phone as on a wide screen.
function KindFilter({ value, onChange, attribute }) {
  return (
    <select {...{ [attribute]: "" }} aria-label="Kind" value={value} onChange={(event) => onChange(event.target.value)}>
      <option value="all">Kind: All</option>
      <option value="frontend">Frontend</option>
      <option value="standard">Standard</option>
    </select>
  );
}

function FrontendBadge({ kind }) {
  return kindOf(kind) === "frontend" ? <StatusPill label="Frontend" icon="monitor" tone="status-running" data-frontend-badge /> : null;
}

function FeatureList({ features, showProject = false, control = null, scope = null }) {
  const [query, setQuery] = useState("");
  const [state, setState] = useState("all");
  const [kind, setKind] = useKindFilter();
  const [page, setPage] = useState(0);
  const [marked, setMarked] = useState([]);
  const [results, setResults] = useState([]);
  const [swiped, setSwiped] = useState(null);
  const [asking, setAsking] = useState(null);
  const [selecting, setSelecting] = useState(false);
  const [scopeShown, setScopeShown] = useState(scope);
  // On a phone the bar is a sheet fixed over the list's end that grows with each project's buttons, Start's
  // question or a refusal, so the list keeps the sheet's measured height free below its last row.
  const [sheetHeight, setSheetHeight] = useState(null);
  const watchSheet = useCallback((bar) => {
    if (bar === null) return undefined;
    const measure = () => setSheetHeight(Math.ceil(bar.getBoundingClientRect().height));
    measure();
    if (typeof ResizeObserver !== "function") return () => setSheetHeight(null);
    const watch = new ResizeObserver(measure);
    watch.observe(bar);
    return () => {
      watch.disconnect();
      setSheetHeight(null);
    };
  }, []);
  const touchAt = useRef(null);
  const press = useRef({ timer: null, from: null, fired: false });
  useEffect(() => () => clearTimeout(press.current.timer), []);
  // Selection mode is a phone's: a wide screen has no Select button and shows its marks anyway, so a long
  // press there is a plain tap, and a phone turned wide leaves the mode with its marks kept.
  const phone = usePhone();
  if (!phone && selecting) setSelecting(false);
  // Another project's list leaves selection mode and drops its marks.
  if (scope !== scopeShown) {
    setScopeShown(scope);
    if (selecting) {
      setSelecting(false);
      setMarked([]);
    }
  }
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
  const shown = unfinishedFirst(features).filter((feature) => (state === "all" || feature.status === state) && matchesKind(kind, feature.kind) && feature.name.toLowerCase().includes(needle));
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
  // On a phone, selection mode shows the marks and turns a tap on a row into a toggle of its mark.
  const enterSelecting = () => {
    setSwiped(null);
    setAsking(null);
    setSelecting(true);
  };
  const leaveSelecting = () => {
    setMarked([]);
    setSelecting(false);
  };
  // A change of filter, state or page leaves selection mode, so no mark is left on a row out of sight.
  const refilter = (change) => {
    change();
    if (selecting) leaveSelecting();
  };
  const tapRow = (feature) => (event) => {
    if (!selecting || !markable(feature) || event.target.matches("[data-mark]")) return;
    event.preventDefault();
    toggle(keyOf(feature));
  };
  // A finger held still on a markable row for LONG_PRESS_MS enters selection mode with that row marked; a move
  // past LONG_PRESS_SLOP pixels or a lift first is a scroll or a tap. The click the lift sends is swallowed so the
  // press does not also open the feature.
  const stopPress = () => {
    clearTimeout(press.current.timer);
    press.current.timer = null;
    press.current.from = null;
  };
  const rowTouch = (feature) => {
    const swipe = swipeHandlers(touchAt, (left) => setSwiped(left ? keyOf(feature) : null));
    return {
      onTouchStart: (event) => {
        swipe.onTouchStart(event);
        stopPress();
        press.current.fired = false;
        const point = event.touches[0];
        if (!point || !phone) return;
        press.current.from = { x: point.clientX, y: point.clientY };
        press.current.timer = setTimeout(() => {
          press.current.timer = null;
          press.current.from = null;
          press.current.fired = true;
          const key = keyOf(feature);
          enterSelecting();
          setMarked((now) => (now.includes(key) ? now : [...now, key]));
        }, LONG_PRESS_MS);
      },
      onTouchMove: (event) => {
        const from = press.current.from;
        const point = event.touches[0];
        if (from && point && Math.hypot(point.clientX - from.x, point.clientY - from.y) > LONG_PRESS_SLOP) stopPress();
      },
      onTouchEnd: (event) => {
        stopPress();
        swipe.onTouchEnd(event);
      },
      onTouchCancel: stopPress,
    };
  };
  // A press whose lift sent no click must not swallow the next tap's.
  const forgetPress = () => {
    press.current.fired = false;
  };
  const swallowPressClick = (event) => {
    if (!press.current.fired) return;
    press.current.fired = false;
    event.preventDefault();
    event.stopPropagation();
  };
  const selectedCount = groups.reduce((sum, group) => sum + group.promote.length + group.start.length, 0);
  const finished = (group) => (done) => {
    setMarked([]);
    setResults(done);
    if (done.some((result) => result.ok)) {
      setSelecting(false);
      group.control.onDone();
    }
  };
  return (
    <div className={selecting ? "feature-list selecting" : "feature-list"} style={selecting && sheetHeight !== null ? { "--marked-actions-height": sheetHeight + "px" } : undefined}>
      <div className="feature-filters">
        <label className="search-box">
          <Icon name="search" size={18} />
          <input type="search" data-feature-search aria-label="Search features" placeholder="Search features" autoComplete="off" spellCheck={false} value={query} onChange={(event) => refilter(() => { setQuery(event.target.value); setPage(0); })} />
        </label>
        <select data-feature-status aria-label="State" value={state} onChange={(event) => refilter(() => { setState(event.target.value); setPage(0); })}>
          <option value="all">State: All</option>
          {states.map((name) => (
            <option key={name} value={name}>{pillWord(name, name)}</option>
          ))}
        </select>
        <KindFilter attribute="data-feature-kind" value={kind} onChange={(next) => refilter(() => { setKind(next); setPage(0); })} />
        {adder}
        {features.some(markable) ? (
          <button type="button" className="select-mode" data-select-mode aria-pressed={selecting ? "true" : "false"} onClick={selecting ? leaveSelecting : enterSelecting}>{selecting ? "Done" : "Select"}</button>
        ) : null}
      </div>
      {groups.length > 0 ? (
        <div className="marked-actions" ref={watchSheet}>
          {selecting ? <span className="selected-count" data-selected-count aria-live="polite">{selectedCount + " selected"}</span> : null}
          {groups.map((group) => (
            <div className="marked-group" key={group.projectId}>
              {group.promote.length > 0 ? <PromoteMarked names={group.promote} control={group.control} place={group.place} onFinished={finished(group)} /> : null}
              {group.start.length > 0 ? <StartMarked names={group.start} control={group.control} place={group.place} onFinished={finished(group)} /> : null}
              {group.start.length > 0 ? <QueueMarked names={group.start} control={group.control} place={group.place} onFinished={finished(group)} /> : null}
              {group.start.length > 0 ? <BlockedNote reason={blockedOf(group.control)} /> : null}
            </div>
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
                <tr className={"feature" + (swiped === keyOf(feature) ? " swiped" : "") + (marked.includes(keyOf(feature)) ? " marked" : "")} data-feature-row={feature.name} onClick={tapRow(feature)} onClickCapture={swallowPressClick} {...(markable(feature) && !selecting ? rowTouch(feature) : { onTouchStart: forgetPress })}>
                  <td>
                    {markable(feature) ? <input type="checkbox" className="feature-mark" data-mark={feature.name} aria-label={"Mark " + feature.name} checked={marked.includes(keyOf(feature))} onChange={() => toggle(keyOf(feature))} /> : null}
                    <a className="feature-link" href={featureHref(feature.projectId, feature.name)} data-feature-open={feature.name} title={showProject ? feature.projectName : undefined} onClick={(event) => { if (selecting && markable(feature)) return; if (isPlainLeftClick(event)) { event.preventDefault(); navigate(featureHref(feature.projectId, feature.name)); } }}>{feature.name}</a>
                    <FrontendBadge kind={feature.kind} />
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
      <Pager at={at} pages={pages} onPage={(next) => refilter(() => setPage(next))} />
    </div>
  );
}

// One feature: its status, description, dependencies (each a link to that feature), the runs made
// for it, and the action to start one when it is pending or to promote it when it is a draft.
const FEATURE_TABS = [["description", "Description"], ["test", "Test file"], ["dependencies", "Dependencies"], ["runs", "Runs"]];

// A draft moved from Todo with "Queue once approved" says so, with a Cancel that drops the wish.
function QueueWish({ feature, control }) {
  const [wished, setWished] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const path = API_PROJECTS + "/" + encodeURIComponent(control.projectId) + "/features/" + encodeURIComponent(feature.name) + "/queue-wish";
  useEffect(() => {
    const controller = new AbortController();
    (async () => {
      try {
        const response = await get(path, control.token, controller.signal);
        const body = response.ok ? await jsonOrNull(response) : null;
        if (!controller.signal.aborted) setWished(body !== null && body.queue_when_approved === true);
      } catch (unreachable) {
        // Unknown: nothing is said.
      }
    })();
    return () => controller.abort();
  }, [path]);
  if (!wished) return null;
  const drop = async () => {
    setBusy(true);
    setNote("");
    const why = await todoSend(path, control.token, control.onSignedOut, { queue_when_approved: false }, "The wish could not be dropped.");
    if (why === undefined) return;
    setBusy(false);
    if (why === null) setWished(false);
    else setNote(why);
  };
  return (
    <div className="queue-wish" data-queue-wish>
      <span>Will be queued when approved</span>
      <button type="button" data-queue-wish-cancel disabled={busy} onClick={drop}>Cancel</button>
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
    </div>
  );
}

// The Frontend switch: sets the feature's kind through the kind action; a refusal leaves it as it was and says why.
function FrontendSwitch({ feature, control }) {
  const [kind, setKind] = useState(kindOf(feature.kind));
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const path = API_PROJECTS + "/" + encodeURIComponent(control.projectId) + "/features/" + encodeURIComponent(feature.name) + "/kind";
  const flip = async () => {
    const next = kind === "frontend" ? "standard" : "frontend";
    setBusy(true);
    setNote("");
    const why = await todoSend(path, control.token, control.onSignedOut, { kind: next }, "The kind could not be set.");
    if (why === undefined) return;
    setBusy(false);
    if (why !== null) {
      setNote(why);
      return;
    }
    setKind(next);
    control.onDone();
  };
  return (
    <span className="frontend-switch">
      <label>
        <input type="checkbox" role="switch" data-frontend-switch checked={kind === "frontend"} disabled={busy} onChange={flip} />
        Frontend
      </label>
      {note !== "" ? <span role="status" className="run-action-note error-text">{note}</span> : null}
    </span>
  );
}

function FeatureDetail({ feature, features, runs, projectId, control }) {
  const [tab, setTab] = useState("description");
  // Whether the Improve draft popup is open.
  const [improving, setImproving] = useState(false);
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
            {feature.status === "pending" || feature.status === "draft" ? <FeatureAction key={feature.status} feature={feature} control={control} blockedNote={false} /> : null}
            {feature.status === "pending" ? <LaunchAction feature={feature} control={control} /> : null}
            {feature.status === "pending" ? <QueueAction feature={feature} control={control} /> : null}
            <FeatureEditor key={feature.name} name={feature.name} features={features} control={control} />
            {feature.status === "draft" ? <button type="button" data-improve-draft onClick={() => setImproving(true)}><Icon name="doc" size={16} />Improve draft</button> : null}
            <FrontendSwitch key={feature.name + "/" + kindOf(feature.kind)} feature={feature} control={control} />
          </div>
        ) : null}
        {control !== null && feature.status === "pending" ? <BlockedNote reason={blockedOf(control)} /> : null}
        <p className="muted" data-design-package>Design package: <code>{"designs/" + feature.name + "/"}</code></p>
        {control !== null && improving ? <DraftWithOrqDialog draft={{ project: projectId, name: feature.name }} token={control.token} onSignedOut={control.onSignedOut} onClose={() => setImproving(false)} /> : null}
        {control !== null && feature.status === "draft" ? <QueueWish key={feature.name} feature={feature} control={control} /> : null}
        <div className="feature-tabs" role="tablist" aria-label="Feature details">
          {FEATURE_TABS.map(([id, label]) => (
            <button key={id} type="button" role="tab" id={"feature-tab-" + id} aria-selected={tab === id ? "true" : "false"} aria-controls="feature-panel" data-feature-tab={id} onClick={() => setTab(id)}>{label}</button>
          ))}
        </div>
        <div className="feature-panel" id="feature-panel" role="tabpanel" aria-labelledby={"feature-tab-" + tab}>
          {tab === "description" ? (feature.description ? <Markdown text={feature.description} /> : <p className="muted">No description.</p>) : null}
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

// Reads one list from every readable project side by side; toRows turns a project's parsed reply
// into rows, and a project that cannot be read is counted (and named in `failed`) rather than shown.
function useEveryProject({ token, projects, reloads, onSignedOut, part, parse, toRows }) {
  const [state, setState] = useState({ loading: true, rows: [], read: 0, unread: 0, failed: [] });
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
      const read = results.filter((rows) => rows !== null);
      const failed = readable.filter((project, at) => results[at] === null).map((project) => project.id);
      setState({ loading: false, rows: read.flat(), read: read.length, unread: results.length - read.length, failed });
    })();
    return () => {
      current = false;
      controller.abort();
    };
  }, [token, key, reloads]);
  return state;
}

// Each row carries its own project's control, so a project that allows actions offers them here too.
function AllFeatures({ token, projects, reloads, onSignedOut, onRefresh, onRows }) {
  const toRows = (parsed, project) => {
    const control = parsed.actions ? { token, projectId: project.id, onSignedOut, onDone: onRefresh, startBlocked: startBlockedOf(project) } : null;
    return tagged(parsed.rows, project.id, project.name).map((feature) => ({ ...feature, control }));
  };
  const state = useEveryProject({ token, projects, reloads, onSignedOut, part: "features", parse: parseFeatures, toRows });
  // No count when every project's ledger failed: "0 in progress" would claim what nobody knows.
  useEffect(() => {
    if (!state.loading) onRows(state.read === 0 && state.unread > 0 ? null : state.rows);
  }, [state.loading, state.rows, state.read, state.unread]);
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

// The Features page across every project: the header with the in-progress pill, the project picker,
// and the merged ledgers.
function AllFeaturesPage({ token, reloads, onRefresh, onSignedOut }) {
  const [rows, setRows] = useState(null);
  return (
    <Loaded key={API_PROJECTS} reloads={reloads} header={() => <PageHeader title="Features" subtitle="Every project's feature ledger" badge={inProgressBadge(rows)} onRefresh={onRefresh} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => (
      <>
        <ProjectSelect projects={projects} projectId={null} />
        <AllFeatures token={token} projects={projects} reloads={reloads} onSignedOut={onSignedOut} onRefresh={onRefresh} onRows={setRows} />
      </>
    )} onSignedOut={onSignedOut} />
  );
}

// The features moved out of the ledger into the project's archive, read-only. Null when they cannot be read.
function archivedPath(projectId) {
  return API_PROJECTS + "/" + encodeURIComponent(projectId) + "/features/archived";
}

// { list, failed, retry }: list is null while loading or after a failure; failed says which; a refused token signs out.
function useArchived(token, projectId, reloads, onSignedOut) {
  const [state, setState] = useState({ list: null, failed: false });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setState({ list: null, failed: false });
    (async () => {
      try {
        const response = await get(archivedPath(projectId), token, controller.signal);
        if (controller.signal.aborted) return;
        if (response.status === 401) {
          onSignedOut();
          return;
        }
        const body = response.ok ? await jsonOrNull(response) : null;
        if (controller.signal.aborted) return;
        if (body !== null && Array.isArray(body.archived)) setState({ list: body.archived.filter((entry) => entry !== null && typeof entry.name === "string"), failed: false });
        else setState({ list: null, failed: true });
      } catch (unreachable) {
        if (!controller.signal.aborted) setState({ list: null, failed: true });
      }
    })();
    return () => controller.abort();
  }, [token, projectId, reloads, attempt]);
  return { list: state.list, failed: state.failed, retry: () => setAttempt(attempt + 1) };
}

// The archive filter: "Archived (N)" opens a searchable, read-only list of the archived features.
function ArchivedList({ archived, projectId }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  if (archived === null || archived.length === 0) return null;
  const needle = query.trim().toLowerCase();
  const shown = archived.filter((entry) => needle === "" || entry.name.toLowerCase().includes(needle) || (typeof entry.description === "string" && entry.description.toLowerCase().includes(needle)));
  return (
    <div className="archived-features card">
      <button type="button" data-archived-toggle aria-expanded={open} onClick={() => setOpen(!open)}>{"Archived (" + archived.length + ")"}</button>
      {open ? (
        <>
          <input type="search" data-archived-search aria-label="Search archived features" placeholder="Search archived features" autoComplete="off" spellCheck={false} value={query} onChange={(event) => setQuery(event.target.value)} />
          {shown.length === 0 ? <p>No archived features match.</p> : (
            <ul className="archived-rows">
              {shown.map((entry) => (
                <li key={entry.name} data-archived-row={entry.name}>
                  <a className="feature-link" href={featureHref(projectId, entry.name)} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(featureHref(projectId, entry.name)); } }}>{entry.name}</a>
                  <FeatureState status={entry.status} />
                </li>
              ))}
            </ul>
          )}
        </>
      ) : null}
    </div>
  );
}

// Over this many completed features the page offers to archive them.
const ARCHIVE_OVER = 200;

// "Archive completed (M)": reads what a dry run would move, asks with those counts, then archives.
function ArchiveCompleted({ completed, control }) {
  const [asking, setAsking] = useState(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const path = controlPath(control, "features/archive");
  const send = async (dry) => {
    const response = await actionRequest(path, control.token, { dry_run: dry });
    if (response.status === 401) {
      control.onSignedOut();
      return undefined;
    }
    return { ok: response.ok, body: await jsonOrNull(response) };
  };
  const check = async () => {
    setBusy(true);
    setNote("");
    try {
      const result = await send(true);
      if (result === undefined) return;
      if (result.ok && result.body !== null && typeof result.body.archived === "number") setAsking(result.body);
      else setNote(result.body !== null && typeof result.body.error === "string" ? result.body.error : "The archive could not be checked.");
    } catch (unreachable) {
      setNote("The archive could not be checked.");
    }
    setBusy(false);
  };
  const confirm = async () => {
    setBusy(true);
    try {
      const result = await send(false);
      if (result === undefined) return;
      setAsking(null);
      if (result.ok) {
        setNote("Archived " + (result.body !== null && typeof result.body.archived === "number" ? result.body.archived : 0) + " features.");
        control.onDone();
      } else setNote(result.body !== null && typeof result.body.error === "string" ? result.body.error : "The features could not be archived.");
    } catch (unreachable) {
      setNote("The features could not be archived.");
    }
    setBusy(false);
  };
  const stripped = asking !== null && Array.isArray(asking.stripped) ? asking.stripped.length : 0;
  return (
    <div className="archive-completed">
      {asking === null ? <button type="button" data-archive-completed disabled={busy} onClick={check}>{"Archive completed (" + completed + ")"}</button> : (
        <Confirm question={"Archive " + asking.archived + " features?"} detail={stripped > 0 ? " Their names are dropped from " + stripped + " feature" + (stripped === 1 ? "" : "s") + "' dependencies." : null} answer="Yes, archive" busy={busy} onConfirm={confirm} onCancel={() => setAsking(null)} />
      )}
      {note !== "" ? <span role="status" className="run-action-note">{note}</span> : null}
    </div>
  );
}

// An archived feature's page: what it was, read-only. Not found when the archive has no such feature either.
function ArchivedDetail({ token, projectId, name, reloads, onSignedOut }) {
  const { list: archived, failed, retry } = useArchived(token, projectId, reloads, onSignedOut);
  if (failed) return <p role="alert">The archived features could not be read. <button type="button" data-archived-retry onClick={retry}>Retry</button></p>;
  if (archived === null) return <p>Loading the feature…</p>;
  const feature = archived.find((entry) => entry.name === name);
  if (feature === undefined) return <NotFound back={"/projects/" + encodeURIComponent(projectId) + "/features"} />;
  const deps = Array.isArray(feature.depends_on) ? feature.depends_on : [];
  return (
    <div className="card archived-feature" data-archived-feature={name}>
      <h2>{name}</h2>
      <p><FeatureState status={feature.status} /> <span className="muted">Archived, read-only</span></p>
      {typeof feature.description === "string" ? <p>{feature.description}</p> : null}
      {deps.length > 0 ? <p className="muted">{"Depended on: " + deps.join(", ")}</p> : null}
      {typeof feature.archived_at === "string" ? <p className="muted">{"Archived " + feature.archived_at}</p> : null}
    </div>
  );
}

// The list with the archive beside it: the Archived filter, and the archive action when it applies.
function ArchiveSection({ data, control, token, projectId, reloads, onSignedOut, children }) {
  const { list: archived } = useArchived(token, projectId, reloads, onSignedOut);
  const completed = data.rows.filter((feature) => feature.status === "completed").length;
  return (
    <>
      {control !== null && completed > ARCHIVE_OVER ? <ArchiveCompleted completed={completed} control={control} /> : null}
      {children}
      <ArchivedList archived={archived} projectId={projectId} />
    </>
  );
}

// The Features pages of one project: the list, or one feature's details when its name is given.
function FeaturesPage({ token, onSignedOut, projectId, projects, name, reloads, onRefresh }) {
  const runs = useFeatureRuns(token, projectId, reloads, name !== null);
  const featuresPath = "/api/projects/" + encodeURIComponent(projectId) + "/features";
  const control = (data) => (data.actions ? { token, projectId, onSignedOut, onDone: onRefresh, startBlocked: data.startBlocked, restartPending: data.restartPending } : null);
  return (
    <>
      <Loaded key={featuresPath} reloads={reloads} header={(state) => (
        <>
          <PageHeader title={name === null ? "Features" : "Feature"} subtitle={name === null ? "The project's feature ledger" : name} badge={name === null ? inProgressBadge(state.data?.rows) : null} onRefresh={onRefresh} />
          <ProjectSelect projects={projects} projectId={projectId} />
        </>
      )} token={token} path={featuresPath} loading="Loading the features…" parse={parseFeatures} onSignedOut={onSignedOut} draw={(data) => {
        if (name === null) return <ArchiveSection data={data} control={control(data)} token={token} projectId={projectId} reloads={reloads} onSignedOut={onSignedOut}><FeatureList features={tagged(data.rows, projectId, projectId)} control={control(data)} scope={projectId} /></ArchiveSection>;
        const feature = data.rows.find((entry) => entry.name === name);
        return feature === undefined ? <ArchivedDetail token={token} projectId={projectId} name={name} reloads={reloads} onSignedOut={onSignedOut} /> :<FeatureDetail feature={feature} features={data.rows} runs={runs} projectId={projectId} control={control(data)} />;
      }} />
    </>
  );
}

const RUN_STATES = { running: "Running", accepting: "Accepting", completed: "Completed", failed: "Failed", blocked: "Blocked", needs_review: "Needs review", pause_requested: "Pause requested", paused: "Paused", resumed: "Resumed", design_files_needed: "Design files needed" };

function isOrchestration(run) {
  const text = (value) => value === null || typeof value === "string";
  return run !== null && typeof run === "object" && typeof run.run_id === "string" && typeof run.feature === "string" && typeof run.state === "string" && text(run.stage) && text(run.started_at) && text(run.updated_at) && text(run.target_branch);
}

function parseOrchestrations(data) {
  return data && Array.isArray(data.runs) && data.runs.every(isOrchestration) ? { rows: data.runs, actions: data.remote_actions === true } : null;
}

// The states a run is shown in, needs-attention first, as the state filter offers them.
const STATE_ORDER = ["running", "accepting", "pause_requested", "paused", "needs_review", "blocked", "failed", "completed", "resumed"];

// The state a run is shown, filtered and grouped in. A run merged by hand finished like a completed
// one, so it is Completed; how it was delivered is said beside the pill, never in it.
function shownState(state) {
  return state === "merged" ? "completed" : state;
}

// By when each run last moved (its update, else its start), newest first or oldest first. The sort
// is stable, so runs with the same time keep the server's order.
function byUpdated(runs, order) {
  const when = (run) => whenOf(run.updated_at ?? run.started_at ?? null);
  return [...runs].sort((a, b) => (order === "asc" ? when(a) - when(b) : when(b) - when(a)));
}

// A run's state as a coloured pill. A state the page does not know is "Unknown", never a success.
const RUN_ICONS = { running: "play", accepting: "clock", completed: "check", failed: "close", blocked: "close", needs_review: "doc", pause_requested: "clock", paused: "clock", resumed: "rotate", design_files_needed: "doc" };

// How a completed run that was delivered without a review was delivered: pushed, or merged into its base.
const DELIVERED = { pushed: "Pushed", merged: "Merged locally" };

// How a run delivers its work when it finishes, as its project's settings name it. Push is no longer
// offered; a run recorded with it still says what it was.
const RUN_DELIVERIES = { review: "Open review", push: "Push only (removed)", merge_local: "Merge locally" };

// The delivery a run's page shows: how a finished run was delivered, else the mode its project
// delivers by, or null when the run has none the page knows. An ending the page does not know
// falls back to the mode.
function deliveryOf(run) {
  if (run.state === "merged") return "Merged by hand";
  if (run.state === "completed" && typeof run.ending === "string" && Object.hasOwn(DELIVERED, run.ending)) return DELIVERED[run.ending];
  return typeof run.delivery === "string" && Object.hasOwn(RUN_DELIVERIES, run.delivery) ? RUN_DELIVERIES[run.delivery] : null;
}

// How a finished run was delivered, as the quiet line under its pill in the list, or null when it is
// not finished or the page does not know how.
function endingOf(run) {
  if (run.state === "merged") return "merged by hand";
  if (run.state === "resumed") return typeof run.resumed_as === "string" ? "continued as " + run.resumed_as : null;
  return run.state === "completed" && typeof run.ending === "string" && Object.hasOwn(DELIVERED, run.ending) ? DELIVERED[run.ending].toLowerCase() : null;
}

function RunState({ state }) {
  const shown = shownState(state);
  const known = Object.hasOwn(RUN_STATES, shown);
  // The run-<state> class is a hook for layout and tests only; the pill's look comes from its tone.
  return <StatusPill kind={known ? shown : undefined} label={known ? RUN_STATES[shown] : "Unknown"} icon={known ? RUN_ICONS[shown] : "flag"} className={"run-state run-" + (known ? shown : "unknown")} />;
}

// The pill beside the Orchestrations title: how many of the listed runs are running now.
function runningBadge(rows) {
  if (!Array.isArray(rows)) return null;
  const running = rows.filter((run) => run.state === "running").length;
  return <StatusPill kind="running" label={running + " running"} className="running-badge" data-running-count />;
}

// The pill beside the Features title: how many of the ledger's features are in progress, whatever
// the filters show.
function inProgressBadge(rows) {
  if (!Array.isArray(rows)) return null;
  const busy = rows.filter((feature) => feature.status === "in_progress").length;
  return <StatusPill kind="in_progress" label={busy + " in progress"} className="running-badge" data-in-progress-count />;
}

function runHref(projectId, runId) {
  return "/projects/" + encodeURIComponent(projectId) + "/orchestrations/" + encodeURIComponent(runId);
}

// The runs a resume joined this one to: the run that continued it, and the run it continued. Each is a link
// to that run's page when its id is one, and plain text otherwise; null when the run names neither.
function ResumeLinks({ run, listPath }) {
  const linked = (id) => {
    if (!RUN_ID.test(id)) return id;
    const href = listPath + "/" + encodeURIComponent(id);
    return <a href={href} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>{id}</a>;
  };
  const parts = [];
  if (run.state === "resumed" && typeof run.resumed_as === "string") parts.push(<span key="as" data-resumed-as>{"Continued as "}{linked(run.resumed_as)}</span>);
  if (typeof run.resumed_from === "string") parts.push(<span key="from" data-resumed-from>{"Resumed from "}{linked(run.resumed_from)}</span>);
  return parts.length === 0 ? null : <p className="feature-detail-sub run-detail-sub run-resume-links">{parts}</p>;
}

// The answer the person who resumed this run gave its session, as the server shows it (redacted), as plain
// text; null when it was given none.
function ResumeAnswer({ run }) {
  if (typeof run.answer !== "string" || run.answer === "") return null;
  return (
    <div className="feature-detail-sub run-detail-sub run-answer" data-run-answer>
      <span className="muted">Answered when resumed:</span>
      <p className="run-answer-text" style={{ whiteSpace: "pre-wrap", margin: 0 }}>{run.answer}</p>
    </div>
  );
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
  const [kind, setKind] = useKindFilter();
  if (runs.length === 0) {
    return <p>{showProject ? "No runs in any project yet." : "No runs in this project yet."}</p>;
  }
  const projectOf = (run) => run.projectId ?? projectId;
  const controlOf = (run) => (run.control === undefined ? control : run.control);
  const known = (run) => Object.hasOwn(RUN_STATES, shownState(run.state));
  const states = STATE_ORDER;
  const stages = [...new Set(runs.map((run) => run.stage).filter((name) => name))];
  const needle = query.trim().toLowerCase();
  const matches = runs.filter((run) => (state === "all" || (state === "unknown" ? !known(run) : shownState(run.state) === state)) && (stage === "all" || run.stage === stage) && matchesKind(kind, run.feature_kind) && (run.feature.toLowerCase().includes(needle) || run.run_id.toLowerCase().includes(needle)));
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
        <KindFilter attribute="data-run-kind" value={kind} onChange={(next) => { setKind(next); setPage(0); }} />
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
                    <FrontendBadge kind={run.feature_kind} />
                    <span className="run-id">{run.run_id + (run.target_branch ? " into " + run.target_branch : "") + (showProject ? " · " + run.projectName : "")}</span>
                    {run.kind === "plan" || projectOf(run) === null ? null : <FeatureLink className="run-feature-link" projectId={projectOf(run)} name={run.feature}>Feature</FeatureLink>}
                  </td>
                  <td>
                    <RunState state={run.state} />
                    {endingOf(run) === null ? null : <div className="run-ending muted" data-run-ending>{endingOf(run)}</div>}
                    <EndingFirst run={run} />
                    {run.unattended === true ? <div className="run-ending muted" data-run-unattended>Unattended (queue)</div> : null}
                    {run.kind === "plan" ? <StatusPill kind="draft" label="Draft with orq" data-run-plan /> : null}
                    <DirtyCheckout run={run} />
                  </td>
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
// A run waiting for design files may be accepted as it stands, as one waiting on a review decision.
const ACCEPTABLE = ["failed", "needs_review", "design_files_needed"];

const SET_ASIDE_QUESTION = "Your changes to these files are stashed, the run is merged, and your changes are put back. If they conflict, they stay in the stash and orq tells you where.";
const DIRTY_SHOWN = 10;

// The files a blocked run waits on because the checkout has uncommitted changes to them, or null when it
// waits on nothing of the kind.
function dirtyFiles(run) {
  if (run.state !== "blocked" || !Array.isArray(run.dirty_files)) return null;
  const files = run.dirty_files.filter((file) => typeof file === "string");
  return files.length === 0 ? null : files;
}

// "Waiting for you" on a run blocked by uncommitted changes in the checkout: what is in the way and the files,
// ten at most; its actions (Resume, and Set aside my changes and merge) are the run's own.
function DirtyCheckout({ run }) {
  const files = dirtyFiles(run);
  if (files === null) return null;
  const target = typeof run.target_branch === "string" && run.target_branch !== "" ? run.target_branch : "main";
  return (
    <div className="banner waiting-banner" role="status" data-dirty-checkout>
      <Icon name="alert" size={18} />
      <div>
        <strong>Waiting for you</strong>
        <p>{target + " has uncommitted changes to files this run changes"}</p>
        <ul className="dirty-files">
          {files.slice(0, DIRTY_SHOWN).map((file, at) => (
            <li key={at}><code>{file}</code></li>
          ))}
          {files.length > DIRTY_SHOWN ? <li className="muted">{"and " + (files.length - DIRTY_SHOWN) + " more"}</li> : null}
        </ul>
      </div>
    </div>
  );
}

// What the run's accepts say: while its tests run, that and since when; after the latest failed, why with the end
// of the output, so it can be fixed or discarded; after one passed, what is next by how the run delivers. Accept
// never merges by itself.
function AcceptNotes({ run, runPath }) {
  if (run.state === "accepting") {
    return (
      <p className="accept-progress" role="status" data-accepting>
        {"Running the tests in the worktree (started "}<Moment at={run.accepting_since} />{")"}
      </p>
    );
  }
  const failure = run.accept_failure;
  if (ACCEPTABLE.includes(run.state) && failure !== null && typeof failure === "object" && typeof failure.reason === "string") {
    const { tests, more } = failedTestsOf(failure);
    const output = typeof failure.output === "string" ? failure.output : "";
    const log = typeof failure.log === "string" && LOG_FILE.test(failure.log) && typeof runPath === "string" ? failureLogAddress(runPath, { file: failure.log, line: null }) : null;
    return (
      <div className="banner error-banner accept-failure" role="alert" data-accept-failure>
        <Icon name="alert" size={18} />
        <div className="accept-failure-body">
          <strong>{"Accept failed: " + failure.reason}</strong>
          <FailedTests tests={tests} more={more} />
          {log === null ? null : (
            <p className="accept-log">
              <a href={log} data-accept-log onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(log); } }}>Open the full log</a>
            </p>
          )}
          {output === "" ? null : (
            // The end of the output says it all when no test is named (a build error); otherwise it is there if wanted.
            <details className="accept-output-tail" data-accept-tail open={tests.length === 0}>
              <summary>The end of the output</summary>
              <pre className="accept-output" data-accept-output tabIndex={0}>{output}</pre>
            </details>
          )}
        </div>
      </div>
    );
  }
  if (run.state === "completed" && run.accepted === true) {
    const target = typeof run.target_branch === "string" && run.target_branch !== "" ? run.target_branch : "main";
    const next = run.delivery === "merge_local" ? "Tests passed. Merge into " + target + "?" : "Tests passed. The branch is ready to push and open a review.";
    return <p className="accept-passed" role="status" data-accepted>{next}</p>;
  }
  return null;
}

// The tests a failure names (each `{name, output}`) and how many more failed than are named; data of another
// shape is left out.
function failedTestsOf(failure) {
  const list = failure !== null && typeof failure === "object" && Array.isArray(failure.failed_tests) ? failure.failed_tests : [];
  const tests = list
    .filter((test) => test !== null && typeof test === "object" && typeof test.name === "string" && test.name !== "")
    .map((test) => ({ name: test.name, output: typeof test.output === "string" ? test.output : "" }));
  const more = failure !== null && typeof failure === "object" && Number.isInteger(failure.failed_tests_more) && failure.failed_tests_more > 0 ? failure.failed_tests_more : 0;
  return { tests, more };
}

// The failed tests as a list: each name opens its own output, the first one open, the rest closed.
function FailedTests({ tests, more }) {
  if (tests.length === 0) return null;
  return (
    <ul className="failed-tests" data-failed-tests>
      {tests.map((test, at) => (
        <li key={at}>
          <details className="failed-test" data-failed-test open={at === 0}>
            <summary><code className="failed-test-name">{test.name}</code></summary>
            {test.output === "" ? <p className="field-hint muted">No output was kept.</p> : <pre className="failed-test-output" data-failed-test-output tabIndex={0}>{test.output}</pre>}
          </details>
        </li>
      ))}
      {more > 0 ? <li className="field-hint muted" data-failed-tests-more>{"and " + more + " more"}</li> : null}
    </ul>
  );
}

// Why a blocked or failed run ended, as the server gives it, or null for any other run or a reason of another shape.
function endingReasonOf(run) {
  if (!["blocked", "failed"].includes(run.state)) return null;
  return typeof run.ending_reason === "string" && run.ending_reason !== "" ? run.ending_reason : null;
}

// The findings an escalated review left open, as the ending names them: each `id (severity, category)`, then its
// file:line when the server found one. Entries of another shape are left out.
function endingFindingsOf(run) {
  const list = Array.isArray(run.ending_findings) ? run.ending_findings : [];
  const text = (value) => typeof value === "string" && value !== "";
  return list
    .filter((f) => f !== null && typeof f === "object" && text(f.id) && text(f.severity))
    .map((f) => {
      const named = f.id + " (" + f.severity + (text(f.category) ? ", " + f.category : "") + ")";
      const place = text(f.file) ? " " + f.file + (Number.isInteger(f.line) && f.line >= 1 ? ":" + f.line : "") : "";
      return named + place;
    });
}

// The first sentence of a text: up to the first full stop, question or exclamation mark followed by a space or the
// end, so a dot inside a file name does not end it; the whole text when it has none.
function firstSentence(text) {
  const found = /^[\s\S]*?[.!?](?=\s|$)/.exec(text);
  return (found === null ? text : found[0]).trim();
}

// Under a blocked or failed run's pill in the list: the first sentence of why it ended, on one line, the whole
// reason on hover.
function EndingFirst({ run }) {
  const reason = endingReasonOf(run);
  if (reason === null) return null;
  return <div className="run-ending error-text" data-ending-first title={reason} style={{ maxWidth: "24rem", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{firstSentence(reason)}</div>;
}

// At the top of a blocked or failed run's summary: why it ended, in the error style, with the findings an escalated
// review left open one per line. Not shown when a step's failure already says the same.
function EndingReason({ run, events }) {
  const reason = endingReasonOf(run);
  if (reason === null || events.some((event) => failureOf(event)?.reason === reason)) return null;
  const findings = endingFindingsOf(run);
  return (
    <div className="banner error-banner ending-reason" role="alert" data-ending-reason>
      <Icon name="alert" size={18} />
      <div>
        <strong>{reason}</strong>
        {findings.length === 0 ? null : (
          <ul className="ending-findings">
            {findings.map((line, at) => <li key={at} data-ending-finding>{line}</li>)}
          </ul>
        )}
      </div>
    </div>
  );
}

// A run that ended without its review report, because building it failed: why, and Build it now when the server
// allows actions. Serve also builds it again by itself, so the note goes once the report is there.
function MissingReport({ run, control }) {
  const missing = run.missing_report;
  if (missing === null || typeof missing !== "object" || typeof missing.reason !== "string") return null;
  const build = control === null || !RUN_ID.test(run.run_id) ? null : <RunAction kind="build-report" label="Build it now" icon="rotate" question={"Build the review report of " + run.run_id + " now?"} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/build-report")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Report built."} />;
  return (
    <div className="banner error-banner missing-report" role="note" data-missing-report>
      <Icon name="alert" size={18} />
      <div>
        <strong>{"Review report missing: " + missing.reason}</strong>
        {build}
      </div>
    </div>
  );
}

// The design review of a frontend run, read-only: a round's verdict, per viewport the reference beside each capture,
// the issues grouped by section, then suggestions, the checks not assessed and what blocked the review. The latest
// round shows first; the others open from the round buttons. Images come from the run's image route with the page's
// own auth, as blobs, so a token session sees them too.
const DESIGN_VERDICTS = { pass: "Pass", changes_required: "Changes required", blocked: "Blocked" };
const DESIGN_SEVERITIES = { critical: "Critical", major: "Major", minor: "Minor" };
const DESIGN_STATUSES = { open: "Open", resolved: "Resolved", regressed: "Regressed" };
const DESIGN_IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"];

const textOr = (value, fallback = "") => (typeof value === "string" ? value : fallback);
const listOf = (value) => (Array.isArray(value) ? value : []);
const sizeOf = (width, height) => (Number.isInteger(width) && Number.isInteger(height) ? width + " × " + height : null);

// The run's design rounds as the server shapes them, or null when there are none or they are not that shape.
function designOf(run) {
  const design = run.design;
  if (design === null || typeof design !== "object") return null;
  const rounds = listOf(design.rounds).filter((round) => round !== null && typeof round === "object" && Number.isInteger(round.round));
  return rounds.length === 0 ? null : { of: Number.isInteger(design.of) ? design.of : null, rounds };
}

// The run's design review, read again from the run page each time a design review event arrives live, so a
// round that ends while the page is open shows without a reload. `onFresh` takes each batch of live events. A
// read that fails is tried again every few seconds, a few times, so the last design event of a run still shows.
const DESIGN_REFRESH_TRIES = 5;
function useLiveDesign(run, follow) {
  const [read, setRead] = useState(null);
  const asking = useRef({ controller: null, timer: null });
  const stop = () => {
    if (asking.current.controller !== null) asking.current.controller.abort();
    if (asking.current.timer !== null) clearTimeout(asking.current.timer);
    asking.current = { controller: null, timer: null };
  };
  useEffect(() => {
    setRead(null);
    return stop;
  }, [run]);
  const ask = (tries) => {
    const controller = new AbortController();
    asking.current = { controller, timer: null };
    const again = () => {
      if (controller.signal.aborted || tries <= 1) return;
      asking.current.timer = setTimeout(() => ask(tries - 1), RECONNECT_MS);
    };
    (async () => {
      try {
        const response = await get(follow.base, follow.token, controller.signal);
        if (controller.signal.aborted) return;
        if (response.status === 401) {
          follow.onSignedOut();
          return;
        }
        const body = response.ok ? await response.json() : null;
        if (controller.signal.aborted) return;
        if (body === null || typeof body !== "object" || body.run === null || typeof body.run !== "object") {
          again();
          return;
        }
        const design = designOf(body.run);
        if (design !== null) setRead(design);
      } catch (failure) {
        again();
      }
    })();
  };
  const onFresh = (fresh) => {
    if (follow === null || !fresh.some((event) => event.stage === "design_review")) return;
    stop();
    ask(DESIGN_REFRESH_TRIES);
  };
  return { design: read ?? designOf(run), onFresh };
}

// Issues grouped by section, sections in the order the review first names them.
function bySection(issues) {
  const groups = new Map();
  for (const issue of issues) {
    if (issue === null || typeof issue !== "object") continue;
    const section = textOr(issue.section, "Other") || "Other";
    if (!groups.has(section)) groups.set(section, []);
    groups.get(section).push(issue);
  }
  return [...groups.entries()];
}

// The design images of one run page, each fetched once however often it shows (a reference sits beside every
// capture of its viewport) and kept as a blob URL until the panel goes or shows another run.
function useDesignImages(source) {
  const base = source === null ? null : source.base;
  const images = useMemo(() => ({ controller: new AbortController(), asked: new Map(), urls: [] }), [base]);
  useEffect(() => () => {
    images.controller.abort();
    for (const url of images.urls) URL.revokeObjectURL(url);
  }, [images]);
  return images;
}

// The blob URL of the design image at `path`, or null when it cannot be shown; asked for once per page while
// it is on the way or once it came. One that failed is forgotten, so the next time it shows asks again.
function designImageUrl(images, source, path) {
  if (!images.asked.has(path)) {
    const asking = (async () => {
      try {
        const response = await get(source.base + "/design/image?path=" + encodeURIComponent(path), source.token, images.controller.signal);
        if (response.status === 401) {
          source.onSignedOut();
          return null;
        }
        const data = response.ok ? await response.json() : null;
        if (data === null || typeof data !== "object" || !DESIGN_IMAGE_TYPES.includes(data.content_type) || typeof data.data !== "string") return null;
        const raw = atob(data.data);
        const bytes = new Uint8Array(raw.length);
        for (let at = 0; at < raw.length; at += 1) bytes[at] = raw.charCodeAt(at);
        const url = URL.createObjectURL(new Blob([bytes], { type: data.content_type }));
        images.urls.push(url);
        return url;
      } catch (error) {
        return null;
      }
    })();
    images.asked.set(path, asking);
    asking.then((url) => {
      if (url === null && images.asked.get(path) === asking) images.asked.delete(path);
    });
  }
  return images.asked.get(path);
}

// One design image as a blob URL, fetched with the page's auth through the panel's shared images.
function useDesignImage(images, source, path) {
  const [url, setUrl] = useState(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (source === null || path === "") return undefined;
    let current = true;
    setUrl(null);
    setFailed(false);
    designImageUrl(images, source, path).then((made) => {
      if (!current) return;
      if (made === null) setFailed(true);
      else setUrl(made);
    });
    return () => {
      current = false;
    };
  }, [images, path]);
  return { url, failed };
}

// A reference or capture: a button that opens it large; a capture that failed says why instead.
function DesignImage({ images, source, path, label, kind, failure = null, onEnlarge }) {
  const { url, failed } = useDesignImage(images, failure === null ? source : null, failure === null ? path : "");
  const marker = { ["data-design-" + kind]: "" };
  let body;
  if (failure !== null) body = <p className="design-image-note error-text">{"Not captured: " + failure}</p>;
  else if (url !== null) body = <button type="button" className="design-image-open" aria-label={"Enlarge " + label} onClick={() => onEnlarge({ url, label })}><img src={url} alt={label} /></button>;
  else body = <p className="design-image-note muted">{failed ? "Image not available" : "Loading…"}</p>;
  return (
    <figure className="design-image" {...marker}>
      {body}
      <figcaption>{label}</figcaption>
    </figure>
  );
}

// The tone and icon of each design verdict, severity and status; one the page does not know is queued-grey.
const DESIGN_TONES = {
  pass: ["status-completed", "check"], resolved: ["status-completed", "check"],
  changes_required: ["status-waiting", "alert"], major: ["status-waiting", "alert"], open: ["status-waiting", "doc"],
  blocked: ["status-failed", "close"], critical: ["status-failed", "alert"], regressed: ["status-failed", "close"],
  minor: ["status-queued", "dots"],
};

function DesignPill({ kind, value, words, ...rest }) {
  const known = Object.hasOwn(words, value) && words[value];
  const [tone, icon] = known && Object.hasOwn(DESIGN_TONES, value) ? DESIGN_TONES[value] : ["status-queued", "flag"];
  return <StatusPill label={known ? words[value] : "Unknown"} icon={icon} tone={tone} className={"design-" + kind + " design-" + kind + "-" + (known ? value : "unknown")} {...rest} />;
}

function DesignIssue({ issue }) {
  const severity = textOr(issue.severity);
  const status = textOr(issue.status);
  return (
    <li className="design-issue" data-design-issue={textOr(issue.id)}>
      <div className="design-issue-head">
        {issue.from_person === true ? (
          <StatusPill label="From you" icon="pencil" tone="neutral" className="design-from-person" data-from-person="" />
        ) : null}
        <DesignPill kind="severity" value={severity} words={DESIGN_SEVERITIES} />
        <DesignPill kind="status" value={status} words={DESIGN_STATUSES} />
        {typeof issue.viewport === "string" && issue.viewport !== "" ? <span className="muted">{issue.viewport}</span> : null}
      </div>
      <dl className="design-issue-detail">
        <dt>Expected</dt>
        <dd>{textOr(issue.expected)}</dd>
        <dt>Actual</dt>
        <dd>{textOr(issue.actual)}</dd>
        {typeof issue.needs_file === "string" ? (
          <>
            <dt>Needs</dt>
            <dd>{"needs a file: " + issue.needs_file}</dd>
          </>
        ) : (
          <>
            <dt>Fix direction</dt>
            <dd>{textOr(issue.fix_direction)}</dd>
          </>
        )}
      </dl>
    </li>
  );
}

function DesignReview({ design, source }) {
  const latest = design.rounds.length - 1;
  // null follows the latest round, so a round that arrives live shows; a round picked by hand stays.
  const [picked, setPicked] = useState(null);
  const at = picked === null ? latest : Math.min(picked, latest);
  const [enlarged, setEnlarged] = useState(null);
  const images = useDesignImages(source);
  useEffect(() => {
    if (enlarged === null) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") setEnlarged(null);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [enlarged]);
  const round = design.rounds[at];
  const verdict = textOr(round.verdict);
  const issues = listOf(round.issues);
  const suggestions = listOf(round.suggestions).filter((line) => typeof line === "string");
  const unassessed = listOf(round.not_assessed).filter((check) => check !== null && typeof check === "object");
  const blockers = listOf(round.blockers).filter((line) => typeof line === "string");
  return (
    <section className="design-review" aria-label="Design review" data-design-review>
      <div className="design-review-head">
        <h3>Design review</h3>
        <DesignPill kind="verdict" value={verdict} words={DESIGN_VERDICTS} data-design-verdict />
        <span className="muted" data-design-round>{"Round " + round.round + (design.of === null ? "" : " of " + design.of)}</span>
      </div>
      {design.rounds.length < 2 ? null : (
        <div className="segmented design-rounds" role="group" aria-label="Round">
          {design.rounds.map((each, index) => <button key={each.round} type="button" aria-pressed={index === at ? "true" : "false"} data-design-pick={each.round} onClick={() => { setEnlarged(null); setPicked(index); }}>{"Round " + each.round}</button>)}
        </div>
      )}
      {typeof round.reason === "string" && round.reason !== "" ? <p className="design-reason">{round.reason}</p> : null}
      {listOf(round.viewports).filter((viewport) => viewport !== null && typeof viewport === "object").map((viewport) => {
        const name = textOr(viewport.name);
        const size = sizeOf(viewport.width, viewport.height);
        const captures = listOf(viewport.captures).filter((capture) => capture !== null && typeof capture === "object");
        return (
          <div key={name} className="design-viewport" data-design-viewport={name}>
            <h4>{size === null ? name : name + " · " + size}</h4>
            {(captures.length === 0 ? [null] : captures).map((capture, at) => (
              <div key={at} className="design-pair">
                {typeof viewport.reference === "string" ? <DesignImage images={images} source={source} path={viewport.reference} label={"Reference, " + name} kind="reference" onEnlarge={setEnlarged} /> : <p className="design-image-note muted">No reference</p>}
                {capture === null ? <p className="design-image-note muted">No capture</p> : <DesignImage images={images} source={source} path={textOr(capture.image)} label={"Capture of " + textOr(capture.route, "/") + ", " + name} kind="capture" failure={typeof capture.failure === "string" ? capture.failure : null} onEnlarge={setEnlarged} />}
              </div>
            ))}
          </div>
        );
      })}
      <h4>Issues</h4>
      {issues.length === 0 ? <p className="muted">No issues.</p> : bySection(issues).map(([section, inSection]) => (
        <div key={section} className="design-section" data-design-section={section}>
          <h5>{section}</h5>
          <ul className="design-issues">{inSection.map((issue, at) => <DesignIssue key={at} issue={issue} />)}</ul>
        </div>
      ))}
      {suggestions.length === 0 ? null : (
        <>
          <h4>Suggestions</h4>
          <ul data-design-suggestions>{suggestions.map((line, at) => <li key={at}>{line}</li>)}</ul>
        </>
      )}
      {unassessed.length === 0 ? null : (
        <>
          <h4>Not assessed</h4>
          <ul data-design-not-assessed>{unassessed.map((check, at) => <li key={at}>{textOr(check.id) + (listOf(check.evidence).length === 0 ? "" : ": " + listOf(check.evidence).filter((line) => typeof line === "string").join("; "))}</li>)}</ul>
        </>
      )}
      {blockers.length === 0 ? null : (
        <>
          <h4>Blocked by</h4>
          <ul className="error-text" data-design-blockers>{blockers.map((line, at) => <li key={at}>{line}</li>)}</ul>
        </>
      )}
      {enlarged === null ? null : (
        <div className="design-zoom" role="dialog" aria-modal="true" aria-label={enlarged.label} data-design-zoom onClick={() => setEnlarged(null)}>
          <button type="button" className="design-zoom-close" aria-label="Close" data-design-zoom-close onClick={() => setEnlarged(null)}><Icon name="close" size={20} /></button>
          <img src={enlarged.url} alt={enlarged.label} />
        </div>
      )}
    </section>
  );
}

// A run that stopped for want of design files: each one, where it goes, why it is needed and, for a reference, the
// viewport size; Resume rechecks the folder and the page then shows what is still missing.
function DesignFilesNeeded({ run, control }) {
  const needed = run.design_files_needed;
  if (needed === null || typeof needed !== "object") return null;
  const files = listOf(needed.files).filter((file) => file !== null && typeof file === "object" && typeof file.path === "string");
  const feature = textOr(needed.feature, run.feature);
  const path = (tail) => controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/" + tail);
  const resume = control === null || !RUN_ID.test(run.run_id) ? null : <RunAction kind="resume" tone="primary" icon="rotate" label="Resume" question={(reply) => (typeof reply.next === "string" ? "Resume " + run.run_id + " from " + reply.next + "?" : null)} path={path("resume")} check={path("resume-check")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next, reply, answered) => resumedNote(next, answered)} />;
  return (
    <section className="design-files-needed" aria-label="Design files needed" data-design-files-needed>
      <h3>Design files needed</h3>
      <p>{"Add these to designs/" + feature + "/, then resume."}</p>
      <ul className="design-needed-files">
        {files.map((file) => {
          const viewport = file.viewport !== null && typeof file.viewport === "object" ? sizeOf(file.viewport.width, file.viewport.height) : null;
          return (
            <li key={file.path} data-design-needed-file={file.path}>
              <code>{file.path}</code>
              <span>{textOr(file.what)}</span>
              {typeof file.why === "string" && file.why !== "" ? <span className="muted">{file.why}</span> : null}
              {viewport === null ? null : <span>{"Viewport: " + viewport}</span>}
            </li>
          );
        })}
      </ul>
      {resume}
    </section>
  );
}

// Pause and Stop on a running run, Stop on one asked to pause, Resume on a paused, failed or blocked one and
// Review again on one that needs review, when the server allows actions. Resume asks the server where the run
// would continue before asking. Discard on the runs Resume is on and Clear on a merged one first ask what the
// branch would lose. Accept on a failed or needs_review run asks first; its tests then run on the server, so the
// result shows as the run's state once they finish. Switch model on a paused run picks a role and a profile.
// Set aside my changes and merge, on a run blocked by uncommitted changes to its files, asks first.
// The run's row and its detail header show the same ones. In a row (menu) the first stays in sight, Pause or
// Resume, and the rest sit behind a ⋯ button; on a phone the CSS shows them all in the row instead.
// What a resume's done note says: whether an answer went with it (null for a Review again, which takes none),
// then the run that carries on, when the server named one.
function resumedNote(next, answered) {
  const how = answered === null ? "Resumed" : answered ? "Resumed with your answer" : "Resumed without an answer";
  return next === "" ? how + "." : how + " as run " + next + ".";
}

// The decision a blocked run stopped at, as its ending reason "needs a decision: ..." says it, or null.
function decisionOf(run) {
  const reason = endingReasonOf(run);
  return run.state === "blocked" && reason !== null && reason.startsWith("needs a decision:") ? reason : null;
}

function RowActions({ run, control, menu = false }) {
  if (control === null || !RUN_ID.test(run.run_id)) return null;
  // Its tests are running in the worktree: nothing may change the run under them, so nothing is offered.
  if (run.state === "accepting") return <span className="run-actions muted" data-accept-busy>{"Accept, Review again, Resume and Discard wait: an accept is running"}</span>;
  const pause = run.state === "running" ? <RunAction kind="pause" label="Pause" icon="pause" question={"Pause " + run.run_id + "?"} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/pause")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Pause requested."} /> : null;
  const runPath = (tail) => controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/" + tail);
  const resume = RESUMABLE.includes(run.state) ? <RunAction kind="resume" tone="primary" icon="rotate" answers={run.state !== "needs_review"} decision={decisionOf(run)} label={run.state === "needs_review" ? "Review again" : "Resume"} question={(reply) => (typeof reply.next === "string" ? "Resume " + run.run_id + " from " + reply.next + "?" : null)} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/resume")} check={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/resume-check")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next, reply, answered) => resumedNote(next, run.state === "needs_review" ? null : answered)} /> : null;
  const stop = run.state === "running" || run.state === "pause_requested" ? <RunAction kind="stop" label="Stop" tone="danger" icon="stop" question={"Stop " + run.run_id + "?" + (run.kind === "plan" ? " Claude stops drafting." : " Its process ends and it can be resumed later.")} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/stop")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Stopped."} /> : null;
  // A Draft with orq run has no worktree or branch to pause, resume, accept or discard: it can only be stopped.
  if (run.kind === "plan") return stop === null ? null : <span className="run-actions">{stop}</span>;
  const discard = DISCARDABLE.includes(run.state) ? <RunAction kind="discard" tone="danger" icon="trash" label={run.state === "merged" ? "Clear" : "Discard"} question={(reply) => discardQuestion(run, reply)} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/discard")} check={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/discard-warning")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(gone, reply) => discardDone(gone || run.run_id, reply)} /> : null;
  // A merge-local run that passes its accept is merged into its target; any other is marked review ready.
  const target = typeof run.target_branch === "string" && run.target_branch !== "" ? run.target_branch : "main";
  const merges = run.delivery === "merge_local";
  const acceptQuestion = merges ? "Accept " + run.run_id + "? Its tests run in its worktree; if they pass it is merged into " + target + "." : "Accept " + run.run_id + "? The tests run in its worktree; if they pass it is marked review ready.";
  const acceptDone = (accepted) => "Accepting " + (accepted || run.run_id) + ": its tests are running. " + (merges ? "It is merged into " + target + " when they pass." : "It shows as review ready when they pass.");
  const accept = ACCEPTABLE.includes(run.state) ? <RunAction kind="accept" label="Accept" tone="primary" icon="check" question={acceptQuestion} path={runPath("accept")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={acceptDone} /> : null;
  // A merge-local run accepted before an accept merged such a run: the accept route merges it now.
  const mergeAccepted = run.state === "completed" && run.merge_pending === true ? <RunAction kind="merge-accepted" label={"Merge into " + target} tone="primary" icon="check" question={"Merge " + run.run_id + " into " + target + "? Its tests passed when it was accepted; they run again only if " + target + " moved since."} path={runPath("accept")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Merging " + run.run_id + " into " + target + "."} /> : null;
  // A failed run whose commits after its last review no review saw may be reviewed again from there.
  const reviewAgain = run.state === "failed" && run.reviewable_again === true ? <RunAction kind="review-again" label="Review again" icon="rotate" question={"Review " + run.run_id + " again? A session reviews the commits after its last review and fixes what that review finds."} path={runPath("review-again")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next) => (next === "" ? "Reviewing again." : "Reviewing again as run " + next + ".")} /> : null;
  const queued = (kind, action, label, title, answers = false) => <RunAction kind={kind} label={label} icon="clock" answers={answers} question={"Queue " + title + " " + run.run_id + "? It starts when the project's limits and queue hours allow."} path={controlPath(control, "queue/add")} body={{ action, run: run.run_id }} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Queued " + title + " " + run.run_id + "."} />;
  const queueResume = !RESUMABLE.includes(run.state) ? null : run.state === "needs_review" ? queued("queue-run-review-again", "review_again", "Queue review again", "Review again") : queued("queue-run-resume", "resume", "Queue resume", "Resume", true);
  const queueAccept = ACCEPTABLE.includes(run.state) ? queued("queue-run-accept", "accept", "Queue accept", "Accept") : null;
  const model = run.state === "paused" ? <ModelAction run={run} control={control} /> : null;
  // Only the run page carries the design review, so only there does a design run take a person's issues.
  const designIssues = takesDesignIssues(run) ? <DesignIssuesAction run={run} control={control} /> : null;
  const setAside = dirtyFiles(run) === null ? null : <RunAction kind="merge-set-aside" label="Set aside my changes and merge" icon="check" question={SET_ASIDE_QUESTION} path={controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/merge-set-aside")} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={(next) => (next === "" ? "Merging." : "Merging as run " + next + ".")} />;
  if (pause === null && resume === null && stop === null && discard === null && accept === null && mergeAccepted === null && model === null && setAside === null) return null;
  const all = [pause, resume, reviewAgain, designIssues, setAside, mergeAccepted, model, stop, accept, queueResume, queueAccept, discard].filter((action) => action !== null);
  if (menu && all[0].type === RunAction) all[0] = cloneElement(all[0], { float: true });
  if (!menu || all.length === 1) return <span className="run-actions">{all}</span>;
  return <span className="run-actions">{all[0]}<MoreMenu label={"More actions for " + run.run_id}>{all.slice(1)}</MoreMenu></span>;
}

// A ⋯ button that opens its actions next to it, placed against the window (see placeMenu) so the run table's
// sideways scroll cannot cut them or a question they ask off; Escape or a press elsewhere closes it. The actions stay mounted
// while it is closed, so a question one of them asked is still there when it opens again.
function MoreMenu({ label, children }) {
  const [open, setOpen] = useState(false);
  const box = useRef(null);
  const toggle = useRef(null);
  const menu = useRef(null);
  useOnlyOpen(open, () => setOpen(false));
  useAnchored(open, toggle, menu);
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
      <button type="button" ref={toggle} className="run-more-toggle" data-run-more aria-label={label} title="More actions" aria-haspopup="menu" aria-expanded={open ? "true" : "false"} onClick={() => setOpen(!open)}><Icon name="dots" size={18} /></button>
      <span className="run-more-menu" ref={menu}>{children}</span>
    </span>
  );
}

// The ⋯ menu or floating question open now, as a ref to what closes it: opening another closes it first, so
// only one is ever open.
let openPopup = null;
function useOnlyOpen(open, close) {
  const closer = useRef(close);
  closer.current = close;
  useEffect(() => {
    if (!open) return undefined;
    if (openPopup !== null && openPopup !== closer) openPopup.current();
    openPopup = closer;
    return () => {
      if (openPopup === closer) openPopup = null;
    };
  }, [open]);
}

// Keeps the fixed element `floating` at its `anchor` button while `open` (see placeMenu). It is measured at the
// window's top left: an auto-width fixed element shrinks to the room on its right, so only there is it its own
// width, whatever its place or where it was put last. What it holds may change its size (a question an action
// in a ⋯ menu asks), so it is placed again whenever that changes, and it follows its button when the page or
// the run table scrolls (captured: a scroll does not bubble) or the window changes size.
function useAnchored(open, anchor, floating) {
  useLayoutEffect(() => {
    if (!open || anchor.current === null || floating.current === null) return undefined;
    const place = () => {
      if (anchor.current === null || floating.current === null) return;
      floating.current.style.top = "0px";
      floating.current.style.left = "0px";
      const at = placeMenu(anchor.current.getBoundingClientRect(), floating.current.getBoundingClientRect(), { width: window.innerWidth, height: window.innerHeight });
      floating.current.style.top = at.top + "px";
      floating.current.style.left = at.left + "px";
    };
    place();
    const changes = new MutationObserver(place);
    changes.observe(floating.current, { childList: true, subtree: true, characterData: true });
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      changes.disconnect();
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [open]);
}

// A run table row's question in a panel outside the table, anchored to its action's button like a ⋯ menu,
// so the row and its columns keep their size. Focus moves to its first button; Cancel or Escape closes it
// and puts focus back on the action's button; a press anywhere else closes it. Opening it closes any other
// open panel or ⋯ menu. It is drawn in the page's content, outside the table, so its sideways scroll cannot
// cut it off.
function FloatingQuestion({ anchor, busy, onClose, children }) {
  const panel = useRef(null);
  const close = (refocus) => {
    onClose();
    if (refocus && anchor.current !== null) anchor.current.focus();
  };
  useOnlyOpen(true, () => close(false));
  useAnchored(true, anchor, panel);
  useEffect(() => {
    const first = panel.current === null ? null : panel.current.querySelector("button:not([disabled])");
    if (first !== null) first.focus();
  }, []);
  useEffect(() => {
    if (busy) return undefined;
    const onKeyDown = (event) => {
      if (event.key === "Escape") close(true);
    };
    const onPress = (event) => {
      const inside = (ref) => ref.current !== null && ref.current.contains(event.target);
      if (!inside(panel) && !inside(anchor)) close(false);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPress, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPress, true);
    };
  });
  // Cancel unmounts the panel, so focus would fall to the page: it goes back to the action's button instead.
  const onClick = (event) => {
    if (event.target.closest("[data-cancel]") !== null && anchor.current !== null) anchor.current.focus();
  };
  const host = (anchor.current !== null && anchor.current.closest(".content")) || document.body;
  return createPortal(<div className="run-action-float" data-run-action-float ref={panel} onClick={onClick}>{children}</div>, host);
}

// Where an open ⋯ menu goes in a window `view`: just under its button, right edges together, or just above the
// button when it would run past the window's bottom there and fits above; never past the window's edges, even
// when that means covering the button.
const MENU_GAP = 4;
const MENU_EDGE = 8;
function placeMenu(button, menu, view) {
  const below = button.bottom + MENU_GAP;
  const above = button.top - MENU_GAP - menu.height;
  const fitsBelow = below + menu.height <= view.height - MENU_EDGE;
  const left = Math.max(MENU_EDGE, Math.min(button.right - menu.width, view.width - MENU_EDGE - menu.width));
  const side = fitsBelow || above < MENU_EDGE ? below : above;
  const top = Math.max(MENU_EDGE, Math.min(side, view.height - MENU_EDGE - menu.height));
  return { top, left };
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

// Every readable project's run queue, one group per project in project order: the projects with something
// queued or paused, and those whose queue could not be read. Only shown here; each group links to its
// project's queue page, where it is reordered, removed from and paused, so no order across projects is implied.
function AllQueues({ token, projects, reloads, onSignedOut }) {
  const state = useEveryProject({ token, projects, reloads, onSignedOut, part: "queue", parse: parseQueue, toRows: (parsed, project) => [{ projectId: project.id, queue: parsed.queue }] });
  if (state.loading) {
    return (
      <div className="skeleton" role="status" aria-busy="true">
        <span className="skeleton-bar" aria-hidden="true" />
        <span className="sr-only">Loading the queues…</span>
      </div>
    );
  }
  const groups = projects.flatMap((project) => {
    if (state.failed.includes(project.id)) return [{ project, queue: null }];
    const row = state.rows.find((candidate) => candidate.projectId === project.id);
    return row !== undefined && (row.queue.entries.length > 0 || row.queue.paused !== null) ? [{ project, queue: row.queue }] : [];
  });
  if (groups.length === 0) {
    return <p className="card muted queue-empty" data-queue-empty>Nothing is queued. Queue pending features from a project's Features page.</p>;
  }
  return groups.map(({ project, queue }) => <QueueGroup key={project.id} project={project} queue={queue} />);
}

// One project's queue on the all-projects page, read-only; a null queue is one that could not be read.
function QueueGroup({ project, queue }) {
  const href = "/projects/" + encodeURIComponent(project.id) + "/queue";
  return (
    <section className="card queue-section queue-group" data-queue-group={project.id}>
      <div className="queue-group-head">
        <a className="queue-group-name" href={href} onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>
          <Icon name="folder" />
          <span className="queue-group-title">{project.name}</span>
          <Icon name="chevronRight" />
        </a>
        {queue !== null ? <span className="muted queue-group-count">{queue.entries.length + " queued"}</span> : null}
      </div>
      {queue === null ? <p className="error-text" data-queue-group-error>The queue could not be read.</p> : null}
      {queue !== null && queue.paused !== null ? <QueuePausedBanner reason={queue.paused} projectId={project.id} /> : null}
      {queue !== null && queue.paused === null ? <QueueReviewWait queue={queue} /> : null}
      {queue !== null && queue.entries.length > 0 ? (
        <ol className="queue-list">
          {queueRails(queue.entries).map((rail, index) => (
            <li key={queue.entries[index].feature} className="queue-entry" data-queue-entry={queue.entries[index].feature}>
              <QueueEntryRow entry={queue.entries[index]} position={index + 1} projectId={project.id} rail={rail} controls={null} />
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}

// The Queue page across every project: the header, the project picker, and each project's queue.
function AllQueuesPage({ token, reloads, onRefresh, onSignedOut }) {
  return (
    <Loaded key={API_PROJECTS} reloads={reloads} header={() => <PageHeader title="Queue" subtitle="The features each project's server starts in turn" onRefresh={onRefresh} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseProjects} draw={(projects) => (
      <>
        <ProjectSelect projects={projects} projectId={null} section="queue" />
        <AllQueues token={token} projects={projects.filter((project) => project.available !== false)} reloads={reloads} onSignedOut={onSignedOut} />
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

function EventItem({ event, onLog = null }) {
  const failure = failureOf(event);
  return (
    <li className="event">
      <strong className="event-kind">{event.kind}</strong>
      {event.stage ? <span className="event-meta">{"Stage: " + event.stage}</span> : null}
      <span className="event-meta">{"Attempt " + event.attempt}</span>
      <Moment at={event.occurred_at} />
      {failure !== null && failure.reason !== null ? <span className="event-meta event-failure" data-event-failure>{failure.reason}</span> : null}
      {failure !== null && failure.log !== null && onLog !== null ? <button type="button" className="link-button" data-event-log onClick={() => onLog(failure.log)}>Open log at the error</button> : null}
      {event.mcp ? <McpSetup mcp={event.mcp} /> : null}
      {event.set_aside ? <SetAside setAside={event.set_aside} /> : null}
    </li>
  );
}

// What the server says about a failure event: why it failed and, when it names one of the run's logs, where
// in it the error is (`line` counts from 1, or is null when the server could not give one) and the lines
// around it. Null for any other event or for data that is not that shape; a log that is not usable is left out.
function failureOf(event) {
  const failure = event.kind.endsWith(".failed") ? event.failure : undefined;
  if (failure === null || typeof failure !== "object" || Array.isArray(failure)) return null;
  const reason = typeof failure.reason === "string" && failure.reason !== "" ? failure.reason : null;
  const log = failure.log;
  const usable = log !== null && typeof log === "object" && typeof log.file === "string" && LOG_FILE.test(log.file)
    && (log.line === undefined || log.line === null || (Number.isInteger(log.line) && log.line >= 1))
    && (log.excerpt === undefined || typeof log.excerpt === "string");
  return { reason, log: usable ? { file: log.file, line: Number.isInteger(log.line) ? log.line : null, excerpt: typeof log.excerpt === "string" ? log.excerpt : "" } : null, failed: failedTestsOf(failure) };
}

// The latest failure among some events that the server gave anything for, or, with `withLog`, that names a log.
function latestFailure(events, withLog = false) {
  for (let at = events.length - 1; at >= 0; at -= 1) {
    const failure = failureOf(events[at]);
    if (failure !== null && (!withLog || failure.log !== null)) return failure;
  }
  return null;
}

// How many lines above an error its log opens at, so what led up to it shows too.
const LINES_BEFORE_ERROR = 5;

// A run's Logs address for a failure's log: at its error line, marked, when the server placed it.
function failureLogAddress(runPath, log) {
  const file = runPath + "/logs/" + encodeURIComponent(log.file);
  return log.line === null ? file : file + "/after/" + Math.max(0, log.line - LINES_BEFORE_ERROR) + "/line/" + log.line;
}

// What a set-aside merge did with the checkout's changes: put back, or, when they conflicted, the stash that
// keeps them, the files in conflict and the run's note, shown like a warning.
function SetAside({ setAside }) {
  const texts = (list) => (Array.isArray(list) ? list.filter((item) => typeof item === "string") : []);
  const conflicted = texts(setAside.conflicted);
  if (setAside.restored === true && conflicted.length === 0) return <p className="event-meta set-aside" data-set-aside>Your changes were set aside and put back.</p>;
  const stash = typeof setAside.stash === "string" ? setAside.stash : "";
  const message = typeof setAside.message === "string" ? setAside.message : "";
  return (
    <div className="set-aside set-aside-conflict" role="note" data-set-aside data-set-aside-conflict>
      <Icon name="alert" size={16} />
      <div>
        {stash === "" ? null : <p>{"Your changes are in stash " + stash + (message === "" ? "" : " (" + message + ")")}</p>}
        {conflicted.length === 0 ? null : (
          <ul className="dirty-files">
            {conflicted.map((file, at) => (
              <li key={at}><code>{file}</code></li>
            ))}
          </ul>
        )}
        {typeof setAside.note === "string" && setAside.note !== "" ? <p>{setAside.note}</p> : null}
      </div>
    </div>
  );
}

// The set-aside merges among a run's events, on its timeline: where its changes went.
function SetAsideNotes({ events }) {
  const merged = events.filter((event) => event.set_aside && typeof event.set_aside === "object");
  if (merged.length === 0) return null;
  return (
    <div className="set-aside-notes">
      {merged.map((event) => (
        <SetAside key={event.event_id} setAside={event.set_aside} />
      ))}
    </div>
  );
}

const CLEANUP_STATES = { done: "Done", pending: "Not done yet", skipped: "Skipped", failed: "Failed" };
const CLEANUP_PILLS = { done: ["check", "status-completed"], pending: ["clock", "status-pending"], skipped: ["close", "neutral"], failed: ["close", "status-failed"] };

// What is left to tidy once a run's MR merged, on its page: each step with its state and, when it did not
// finish, why. Clean up runs the steps that are left, after asking; the server takes it only from a passkey
// session, so signed in with the token the card says so instead.
function CleanupCard({ run, control }) {
  const card = run.cleanup;
  if (card === null || typeof card !== "object" || !Array.isArray(card.steps)) return null;
  const target = typeof run.target_branch === "string" && run.target_branch !== "" ? run.target_branch : "the target";
  const labels = { complete_feature: "Mark " + run.feature + " completed", delete_worktree: "Delete the worktree", delete_branch: "Delete the branch", pull_target: "Fast-forward " + target };
  const steps = card.steps.filter((step) => step !== null && typeof step === "object" && typeof labels[step.step] === "string" && typeof CLEANUP_STATES[step.state] === "string");
  const left = steps.some((step) => step.state !== "done");
  const question = "Clean up " + run.run_id + "? Marks " + run.feature + " completed, deletes its worktree and branch if nothing on them would be lost, and fast-forwards " + target + ".";
  const path = control === null ? null : controlPath(control, "runs/" + encodeURIComponent(run.run_id) + "/cleanup");
  return (
    <div className="cleanup-card" data-cleanup>
      <strong>After the merge</strong>
      <ul className="cleanup-steps">
        {steps.map((step) => (
          <li key={step.step} className={"cleanup-step " + step.state} data-cleanup-step={step.step}>
            <span>{labels[step.step]}</span> <StatusPill label={CLEANUP_STATES[step.state]} icon={CLEANUP_PILLS[step.state][0]} tone={CLEANUP_PILLS[step.state][1]} />
            {typeof step.reason === "string" && step.reason !== "" ? <span className="muted">{" " + step.reason}</span> : null}
          </li>
        ))}
      </ul>
      {left && control !== null ? (
        control.token === SESSION
          ? <RunAction kind="run-cleanup" label="Clean up" tone="primary" icon="check" question={question} path={path} body={{ confirm: true }} token={control.token} onSignedOut={control.onSignedOut} onDone={control.onDone} done={() => "Clean up ran."} />
          : <p className="muted" data-cleanup-needs-passkey>Clean up needs a passkey sign-in.</p>
      ) : null}
    </div>
  );
}

// A start that could not pull the run's target, on its timeline: why, and that it began from the local one.
function TargetPullNotes({ events, target }) {
  const warned = events.filter((event) => event.target_pull && typeof event.target_pull === "object" && typeof event.target_pull.message === "string");
  if (warned.length === 0) return null;
  const branch = typeof target === "string" && target !== "" ? target : "the target";
  return (
    <div className="target-pull-notes">
      {warned.map((event) => (
        <p key={event.event_id} className="error-text" role="note" data-run-target-pull>
          <Icon name="alert" size={16} />
          {branch + " was not pulled before this run started: " + event.target_pull.message + ". It started from the local " + branch + "."}
        </p>
      ))}
    </div>
  );
}

// Each push of the target that carried this run's commits, on its timeline: where it went.
function PushedNotes({ events }) {
  const pushes = events.filter((event) => event.kind === "target.pushed" && event.pushed && typeof event.pushed === "object" && typeof event.pushed.remote === "string" && typeof event.pushed.target === "string");
  if (pushes.length === 0) return null;
  return (
    <div className="pushed-notes">
      {pushes.map((event) => (
        <p key={event.event_id} className="run-action-note" role="note" data-run-pushed>
          <Icon name="check" size={16} />
          {"Pushed to " + event.pushed.remote + "/" + event.pushed.target}
        </p>
      ))}
    </div>
  );
}

// The MCP servers the run set up: each one's version and where it came from, and what went wrong.
function McpSetup({ mcp }) {
  const servers = Array.isArray(mcp.servers) ? mcp.servers : [];
  const notes = Array.isArray(mcp.notes) ? mcp.notes : [];
  return (
    <ul className="mcp-setup">
      {servers.map((server, at) => (
        <li key={"s" + at} className="event-meta">{server.name + (server.version ? " " + server.version : "") + (server.source ? " (" + server.source + ")" : "")}</li>
      ))}
      {notes.map((note, at) => (
        <li key={"n" + at} className="event-meta mcp-note">{note}</li>
      ))}
    </ul>
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
        // A design review step is named by its round of the budget, as the server words it.
        title: review ? "Review round " + round : typeof event.design_round === "string" ? event.design_round : event.stage + ", attempt " + event.attempt,
        failed: event.kind.endsWith(".failed"),
        round: review ? round : 0,
        designRound: typeof event.design_round === "string" ? event.design_round : null,
        events: [event],
      });
    }
    between = [];
  }
  for (const loose of between) items.push({ event: loose });
  return items;
}

// The run states in which a run that sent run.completed finished its work rather than stopping short.
const ENDED_STATES = ["completed", "merged", "needs_review"];
// The cleanup step's events: it has no .completed event, so one of these says it finished.
const WORKTREE_OUTCOMES = { "worktree.removed": null, "worktree.kept": "Worktree kept", "worktree.deleted": "Worktree deleted" };

// Whether the run ended without failing: run.completed is among its events and its state says so.
function runEnded(state, events) {
  return ENDED_STATES.includes(state) && events.some((event) => event.kind === "run.completed");
}

// One step's state, the same in the Timeline and the Events tab: failed; done when it completed, when the
// run moved on from it, or when it is the last step of a run that ended; else in progress or not finished.
function stepState(step, { last, ended, running }) {
  if (step.failed) return "failed";
  const completed = step.events.some((event) => event.kind.endsWith(".completed") || (step.stage === "cleanup" && event.kind in WORKTREE_OUTCOMES));
  if (completed || !last || ended) return "done";
  return running ? "current" : "open";
}

// What became of the worktree, from the cleanup step's latest word on it, or null when there is nothing to say.
function cleanupNote(step) {
  if (step.stage !== "cleanup") return null;
  const said = step.events.filter((event) => event.kind in WORKTREE_OUTCOMES);
  return said.length === 0 ? null : WORKTREE_OUTCOMES[said[said.length - 1].kind];
}

// What became of a step's re-run after its failure was reported unrelated, or null when it had none:
// `label` in a few words for its closed line, `text` in full with the summary.
function flakyNote(step) {
  const said = step.events.filter((event) => event.flaky && typeof event.flaky === "object");
  if (said.length === 0) return null;
  const { summary, passed, not_started: notStarted } = said[said.length - 1].flaky;
  const what = typeof summary === "string" && summary !== "" ? ": " + summary : "";
  if (passed !== true && typeof notStarted === "string") return { label: "could not be started", text: "the re-run could not be started (" + notStarted + "); the failure was reported unrelated" + what };
  return passed === true
    ? { label: "passed on a re-run", text: "passed on a re-run; the first failure was reported unrelated" + what }
    : { label: "the re-run failed too", text: "the re-run failed too; the failure was reported unrelated" + what };
}

// A step's mark: failed, finished, or still open (in progress or not finished).
function StepIcon({ state }) {
  const kind = state === "failed" || state === "done" ? state : "open";
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
function EventTimeline({ events, rounds = 0, ended = false, running = false, onLog = null }) {
  const items = groupEvents(events, rounds);
  const lastStep = items.reduce((found, item, at) => (item.events === undefined ? found : at), -1);
  return (
    <ol className="run-events">
      {items.map((item, at) => {
        if (item.events === undefined) return <EventItem key={item.event.event_id} event={item.event} onLog={onLog} />;
        const state = stepState(item, { last: at === lastStep, ended, running });
        return (
          <li key={item.events[0].event_id} className="attempt" data-failed={item.failed ? "true" : undefined} data-step={state}>
            <div className="attempt-head">
              <StepIcon state={state} />
              <h3 className="attempt-title">{item.title}</h3>
              <StepTime events={item.events} />
            </div>
            {item.failed ? <strong className="attempt-outcome">Failed</strong> : null}
            <ol className="attempt-events">
              {item.events.map((event) => (
                <EventItem key={event.event_id} event={event} onLog={onLog} />
              ))}
            </ol>
          </li>
        );
      })}
    </ol>
  );
}

const STEP_STATES = { done: "Done", failed: "Failed", current: "In progress", open: "Not finished" };

// The run's stages as one line of steps: each finished, failed, or still going, with how long it took.
// Each step's state comes from stepState, so it matches the Events tab.
// The tests a step's gate passed only on a retry, from its gate.flaky_tests events: each with a name.
function flakyTests(events) {
  return events
    .filter((event) => event.kind === "gate.flaky_tests" && Array.isArray(event.flaky_tests))
    .flatMap((event) => event.flaky_tests)
    .filter((test) => test && typeof test.name === "string" && test.name !== "");
}

// The files the step's sessions changed outside their features' files, each once, in the order recorded.
function undeclaredFiles(events) {
  const files = events
    .filter((event) => event.kind === "session.undeclared_files" && Array.isArray(event.undeclared_files))
    .flatMap((event) => event.undeclared_files)
    .filter((file) => typeof file === "string" && file !== "");
  return [...new Set(files)];
}

// The files whose formatting orq undid after the step's sessions, outside their features' files, each once.
function formatReverted(events) {
  const files = events
    .filter((event) => event.kind === "session.format_reverted" && Array.isArray(event.format_reverted))
    .flatMap((event) => event.format_reverted)
    .filter((file) => typeof file === "string" && file !== "");
  return [...new Set(files)];
}

// Whether each step detail was opened or closed, by run, step and kind, kept while the page lives so new
// events (which redraw the steps) never close a detail someone opened; a reload starts them as they start.
const shownDetails = new Map();

// One closed line under a step, with a chevron, that opens its detail in place. Its button is marked
// data-step-<kind>-toggle. `startOpen` is how it is shown until someone opens or closes it, so a run
// that turns failed on a refresh opens the step it failed on.
function StepDetail({ place, kind, label, startOpen = false, children }) {
  const key = place + "\n" + kind;
  const [choice, setChoice] = useState(() => (shownDetails.has(key) ? shownDetails.get(key) : null));
  const open = choice === null ? startOpen : choice;
  const toggle = () => {
    shownDetails.set(key, !open);
    setChoice(!open);
  };
  return (
    <div className="run-step-undeclared run-step-detail">
      <button type="button" className="link-button run-step-undeclared-toggle" {...{ ["data-step-" + kind + "-toggle"]: "" }} aria-expanded={open ? "true" : "false"} onClick={toggle}>
        <Icon name={open ? "chevronDown" : "chevronRight"} size={14} />
        <span>{label}</span>
      </button>
      {open ? children : null}
    </div>
  );
}

// A failed step's named tests behind one closed line, "Failed tests: N": opened, each name with its output
// under it (or a note that none was kept), and how many more failed than are named.
function StepFailedTests({ place, tests, more }) {
  if (tests.length === 0) return null;
  const label = "Failed tests: " + tests.length + (more > 0 ? " (and " + more + " more)" : "");
  return (
    <StepDetail place={place} kind="failed-tests" label={label}>
      <ul className="run-step-failed-tests" data-step-failed-tests>
        {tests.map((test, at) => (
          <li key={at}>
            <code className="failed-test-name" data-step-failed-test-name>{test.name}</code>
            {test.output === "" ? (
              <p className="field-hint muted">No output was kept.</p>
            ) : (
              <div className="run-step-excerpt">
                <pre data-step-failed-test-output tabIndex={0} aria-label={"Output of " + test.name}>{test.output}</pre>
              </div>
            )}
          </li>
        ))}
        {more > 0 ? <li className="field-hint muted" data-step-failed-tests-more>{"and " + more + " more"}</li> : null}
      </ul>
    </StepDetail>
  );
}

// The design review step's note for each correction round split into parts, as the server words it.
function designSplits(events) {
  return events.filter((event) => event.kind === "design_correction_split" && typeof event.design_round === "string").map((event) => event.design_round);
}

function RunSteps({ runId, events, rounds, running, ended = false, failedRun = false, onLogs }) {
  const steps = groupEvents(events, rounds).filter((item) => item.events !== undefined);
  // The step a failed run ended on: the last failed one. Its excerpt starts open; every other starts closed.
  const failedOn = failedRun ? steps.reduce((found, step, at) => (step.failed ? at : found), -1) : -1;
  return (
    <ol className="run-steps">
      {steps.map((step, at) => {
        const last = at === steps.length - 1;
        const kind = stepState(step, { last, ended, running });
        const note = cleanupNote(step);
        const flaky = flakyNote(step);
        const took = stepDuration(step.events);
        const undeclared = undeclaredFiles(step.events);
        const reverted = formatReverted(step.events);
        const outside = undeclared.length + reverted.length;
        const flakyList = flakyTests(step.events);
        const place = runId + "\n" + step.events[0].event_id;
        const name = step.round > 0 || step.designRound ? step.title : step.attempt > 1 ? step.stage + " · attempt " + step.attempt : step.stage;
        const failure = kind === "failed" ? latestFailure(step.events) : null;
        const failedLog = failure === null ? null : failure.log;
        return (
          <li key={step.events[0].event_id} className="run-step" data-step={kind}>
            <span className="run-step-mark" aria-hidden="true">{kind === "done" ? <Icon name="check" size={14} /> : kind === "failed" ? <Icon name="close" size={14} /> : null}</span>
            <span className="run-step-name">{name}</span>
            <span className="sr-only">{STEP_STATES[kind]}</span>
            <span className="run-step-time" aria-hidden={kind === "current" ? "true" : undefined}>{kind === "current" ? "In progress" + (took === null ? "" : " (" + took + ")") : took}</span>
            {note === null ? null : <p className="run-cleanup-note field-hint muted">{note}</p>}
            {designSplits(step.events).map((split, n) => (
              <p key={"split" + n} className="run-step-note field-hint muted" data-step-design-split>{split}</p>
            ))}
            {flaky === null ? null : (
              <StepDetail place={place} kind="rerun" label={"Re-run: " + flaky.label}>
                <p className="run-flaky-note field-hint muted">{flaky.text}</p>
              </StepDetail>
            )}
            {flakyList.length > 0 ? (
              <StepDetail place={place} kind="flaky" label={"Flaky tests: " + flakyList.length}>
                <ul className="run-step-undeclared-list">
                  {flakyList.map((test, n) => (
                    <li key={"flaky" + n} data-step-flaky>{"Flaky: " + test.name + (Number.isInteger(test.retry) ? " (passed on retry " + test.retry + ")" : " (passed on a retry)")}</li>
                  ))}
                </ul>
              </StepDetail>
            ) : null}
            {outside > 0 ? (
              <StepDetail place={place} kind="undeclared" label={"Changed outside its features' files: " + outside + (outside === 1 ? " file" : " files")}>
                <ul className="run-step-undeclared-list">
                  {undeclared.map((file) => <li key={file} data-step-undeclared>{file}</li>)}
                  {reverted.map((file) => <li key={"reverted" + file} data-step-format-reverted>{file + " (formatting undone)"}</li>)}
                </ul>
              </StepDetail>
            ) : null}
            {kind === "failed" ? (
              <p className="run-step-failure">
                <Icon name="alert" size={14} />
                <span>{"Failed: " + (failure !== null && failure.reason !== null ? failure.reason : step.events[step.events.length - 1].kind) + " "}<Moment at={step.events[step.events.length - 1].occurred_at} /></span>
                {onLogs ? <button type="button" className="link-button" data-step-logs onClick={() => onLogs(failedLog)}>See the logs</button> : null}
              </p>
            ) : null}
            {kind === "failed" && failure !== null && ALREADY_COMPLETED.test(failure.reason ?? "") ? (
              <p className="run-step-note muted" data-step-completed-note>The feature was already marked completed in the ledger, so the session could not start it again. Resume puts it back to pending and the session carries on under the same name.</p>
            ) : null}
            {failure === null ? null : <StepFailedTests place={place} tests={failure.failed.tests} more={failure.failed.more} />}
            {failedLog !== null && failedLog.excerpt !== "" ? (
              <StepDetail place={place} kind="excerpt" label="Log lines at the error" startOpen={at === failedOn}>
                <div className="run-step-excerpt">
                  <pre data-step-excerpt tabIndex={0} aria-label="Log lines at the error">{failedLog.excerpt}</pre>
                </div>
              </StepDetail>
            ) : null}
          </li>
        );
      })}
    </ol>
  );
}

// A failure whose reason says the feature was already marked 'completed' (the session's init_feature was refused).
const ALREADY_COMPLETED = /already marked ['"]?completed['"]?/i;

const RUN_TABS = [["timeline", "Timeline"], ["logs", "Logs"], ["events", "Events"]];

// One run: its header, then three tabs that switch in place: its steps, its logs, and every event.
// The events are fetched a page at a time; the next page is its own address, so every request stays
// one plain GET. The logs are fetched the first time their tab opens, and kept while it is hidden.
// A draft's fields as Draft with orq shows them, one block per field.
function PlanFields({ fields, ...marks }) {
  const list = (value) => (Array.isArray(value) && value.length > 0 ? value.join(", ") : "None");
  return (
    <div className="plan-fields" {...marks}>
      <p><strong>{fields.name}</strong></p>
      <p className="plan-description">{fields.description}</p>
      <p className="muted">{"Test file: " + (fields.test_file || "None")}</p>
      <p className="muted">{"Target files: " + list(fields.target_files)}</p>
      <p className="muted">{"Depends on: " + list(fields.depends_on)}</p>
    </div>
  );
}

const PLAN_DECISIONS = { replaced: "Replaced: the draft now has Claude's text.", discarded: "Discarded: the draft was left as it was." };

// What a Draft with orq run came to: still drafting, why it failed, the draft a Todo idea became, or an
// improvement shown beside the current draft (side by side where there is room, stacked on a phone) with
// Replace and Discard until one is chosen. Nothing changes the draft before Replace.
function PlanProposal({ plan, projectId, apiRun, control }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  if (plan === null || typeof plan !== "object") return null;
  if (plan.state === "running") return <p className="plan-status" data-plan-status role="status">Claude is drafting…</p>;
  if (plan.state === "failed") return <p className="plan-status error-text" data-plan-status>{typeof plan.reason === "string" && plan.reason !== "" ? plan.reason : "Claude could not draft this."}</p>;
  if (plan.state !== "completed" || plan.proposal === null || typeof plan.proposal !== "object") return null;
  if (plan.source === "todo") {
    const href = projectId === null ? null : featureHref(projectId, plan.feature);
    const text = "Saved as the draft feature " + plan.feature;
    return (
      <p className="plan-status" data-plan-status>
        {href === null ? text : <a href={href} data-plan-feature onClick={(event) => { if (isPlainLeftClick(event)) { event.preventDefault(); navigate(href); } }}>{text}</a>}
      </p>
    );
  }
  const decide = async (leaf) => {
    setBusy(true);
    setNote("");
    let why = "The draft could not be changed.";
    try {
      const response = await actionRequest(apiRun + "/" + leaf, control.token, {});
      if (response.status === 401) {
        control.onSignedOut();
        return;
      }
      const reply = await jsonOrNull(response);
      if (response.ok) {
        setBusy(false);
        control.onDone();
        return;
      }
      if (response.status === 403) why = "Not allowed.";
      else if (reply !== null && typeof reply.error === "string") why = reply.error;
    } catch (unreachable) {
      why = "The server could not be reached.";
    }
    setBusy(false);
    setNote(why);
  };
  const decided = typeof plan.decision === "string" ? PLAN_DECISIONS[plan.decision] || plan.decision : null;
  return (
    <div className="plan-proposal" data-plan-proposal>
      <div className="plan-diff" data-plan-diff style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(16rem, 1fr))", gap: "1rem" }}>
        <section>
          <h3>Current</h3>
          {plan.current !== null && typeof plan.current === "object" ? <PlanFields fields={plan.current} data-plan-current /> : <p className="muted" data-plan-current>No current draft.</p>}
        </section>
        <section>
          <h3>Proposed</h3>
          <PlanFields fields={plan.proposal} data-plan-proposed />
        </section>
      </div>
      {decided !== null ? (
        <p className="plan-status" data-plan-decision>{decided}</p>
      ) : control !== null && apiRun !== null ? (
        <div className="feature-editor-actions">
          <button type="button" className="primary-button" data-plan-replace disabled={busy} onClick={() => decide("plan-replace")}><Icon name="check" size={16} />Replace</button>
          <button type="button" data-plan-discard disabled={busy} onClick={() => decide("plan-discard")}><Icon name="close" size={16} />Discard</button>
        </div>
      ) : null}
      {note !== "" ? <p role="status" className="run-action-note error-text">{note}</p> : null}
    </div>
  );
}

function RunTimeline({ timeline, listPath, runPath, follow, logs, control = null, startTab = "timeline", projectId = null }) {
  const { run, events, next_cursor: next } = timeline;
  const delivery = deliveryOf(run);
  const { design, onFresh } = useLiveDesign(run, follow);
  const [tab, setTab] = useState(startTab);
  const [logsOpened, setLogsOpened] = useState(startTab === "logs");
  const rounds = timeline.review_rounds;
  const live = next === null && follow !== null;
  const choose = (id) => {
    setTab(id);
    if (id === "logs") setLogsOpened(true);
  };
  // A failure's log opens at its error, as an address of its own; without a log the tab opens as it is.
  const openLog = (log) => (log === null ? choose("logs") : navigate(failureLogAddress(runPath, log)));
  const panels = (all) => (
    <>
      <div className="feature-panel" id="run-panel-timeline" role="tabpanel" aria-labelledby="run-tab-timeline" hidden={tab !== "timeline"}>
        <SetAsideNotes events={all} />
        <TargetPullNotes events={all} target={run.target_branch} />
        <PushedNotes events={all} />
        {all.length === 0 ? <p>No events recorded.</p> : <RunSteps runId={run.run_id} events={all} rounds={rounds} running={run.state === "running"} ended={runEnded(run.state, all)} failedRun={run.state === "failed"} onLogs={openLog} />}
      </div>
      <div className="feature-panel" id="run-panel-events" role="tabpanel" aria-labelledby="run-tab-events" hidden={tab !== "events"}>
        {all.length === 0 ? null : <EventTimeline events={all} rounds={rounds} running={run.state === "running"} ended={runEnded(run.state, all)} onLog={openLog} />}
      </div>
    </>
  );
  // The events drawn last, live ones included, kept for the Logs tab, which is drawn after them and
  // reads them once, when it opens.
  const drawn = useRef(events);
  const panelsOf = (all) => {
    drawn.current = all;
    return panels(all);
  };
  return (
    <section className="card feature-detail run-detail">
      <div className="feature-detail-head">
        <button type="button" className="back-arrow" data-back="list" aria-label="Back to the runs" onClick={() => navigate(listPath)}><Icon name="arrowLeft" size={22} /></button>
        <div className="feature-detail-title">
          <div className="feature-detail-line">
            <h2>{projectId !== null && run.kind !== "plan" ? <FeatureLink projectId={projectId} name={run.feature}>{run.feature}</FeatureLink> : run.feature}</h2>
            <RunState state={run.state} />
          </div>
          <EndingReason run={run} events={events} />
          <DirtyCheckout run={run} />
          <AcceptNotes run={run} runPath={runPath} />
          <MissingReport run={run} control={control} />
          <RowActions run={run} control={control} />
          <CleanupCard run={run} control={control} />
          {run.delivery === "merge_local" && follow !== null && projectId !== null ?<Unpushed unpushed={run.unpushed ?? null} push={{ token: follow.token, projectId, onSignedOut: follow.onSignedOut }} /> : null}
          <ResumeLinks run={run} listPath={listPath} />
          <ResumeAnswer run={run} />
          {run.kind === "plan" ? <PlanProposal plan={run.plan === undefined ? null : run.plan} projectId={projectId} apiRun={follow === null ? null : follow.base} control={control} /> : null}
          {validTime(run.started_at) === null && delivery === null ? null : (
            <p className="feature-detail-sub run-detail-sub">
              {validTime(run.started_at) === null ? null : <span>Started <Moment at={run.started_at} /></span>}
              {delivery === null ? null : <span data-run-delivery>{"Delivery: " + delivery}</span>}
            </p>
          )}
        </div>
      </div>
      {run.design_files_needed !== null && typeof run.design_files_needed === "object" ? <DesignFilesNeeded run={run} control={control} /> : design === null ? null : <DesignReview design={design} source={follow === null ? null : { base: follow.base, token: follow.token, onSignedOut: follow.onSignedOut }} />}
      <div className="feature-tabs" role="tablist" aria-label="Run details">
        {RUN_TABS.map(([id, label]) => (
          <button key={id} type="button" role="tab" id={"run-tab-" + id} aria-selected={tab === id ? "true" : "false"} aria-controls={"run-panel-" + id} data-run-tab={id} onClick={() => choose(id)}>{label}</button>
        ))}
      </div>
      {live ? <FollowLive key={follow.base} rounds={rounds} token={follow.token} base={follow.base} from={events.length > 0 ? events[events.length - 1].event_id : follow.cursor} shown={events} onSignedOut={follow.onSignedOut} draw={panelsOf} quiet={tab === "logs"} onFresh={onFresh} /> : panelsOf(events)}
      {next !== null && tab !== "logs" && <button type="button" data-more="events" onClick={() => navigate(runPath + "/after/" + encodeURIComponent(next))}>Load more events</button>}
      {logsOpened && (
        <div className="feature-panel" id="run-panel-logs" role="tabpanel" aria-labelledby="run-tab-logs" hidden={tab !== "logs"}>
          <RunLogs {...logs} runId={run.run_id} preferred={latestFailure(drawn.current, true)?.log.file ?? null} />
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
function FollowLive({ token, base, from, shown, rounds, onSignedOut, draw, quiet = false, onFresh = null }) {
  const [more, setMore] = useState([]);
  const [status, setStatus] = useState("live");
  const [retry, setRetry] = useState(0);
  const [gap, setGap] = useState(false);
  const last = useRef(from);
  // Set while the run is being read again from its start because the place to resume from is gone.
  const restarting = useRef(false);
  // Every event id accepted so far, so a repeat is told apart in constant time.
  const ids = useRef(new Set());
  // Told of each batch of new events; a ref, so the open stream always calls the latest one.
  const freshTo = useRef(onFresh);
  freshTo.current = onFresh;
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
          if (fresh.length > 0) {
            setMore((known) => [...known, ...fresh]);
            if (freshTo.current !== null) freshTo.current(fresh);
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
// A line an address marks, counted from 1.
const MARKED_LINE = /^[1-9][0-9]{0,11}$/;

function parseLogFiles(data) {
  const ok = data && Array.isArray(data.files) && data.files.every((file) => file !== null && typeof file === "object" && typeof file.name === "string" && Number.isInteger(file.bytes));
  return ok ? data.files : null;
}

function parseLogPage(data) {
  const ok = data && typeof data.name === "string" && Array.isArray(data.lines) && data.lines.every((line) => typeof line === "string") && (data.next_cursor === null || (typeof data.next_cursor === "string" && LINE_CURSOR.test(data.next_cursor))) && typeof data.truncated === "boolean";
  return ok ? data : null;
}

// The run's logs inside its Logs tab: the file list is read once, then one file's page at a time.
function RunLogs({ token, onSignedOut, reloads, base, file, cursor, mark = null, runId, preferred = null }) {
  return <Loaded key={base} token={token} path={base} loading="Loading the logs…" parse={parseLogFiles} draw={(files) => <LogsPanel files={files} token={token} onSignedOut={onSignedOut} reloads={reloads} base={base} wanted={file} cursor={cursor} mark={mark} runId={runId} preferred={preferred} />} onSignedOut={onSignedOut} />;
}

// One file's lines with its filters, follow switch and pager. The filters stay while another page of
// the same file opens; choosing another file starts it unfiltered on its first page. Paging keeps the
// cursors already passed, so Prev goes back without asking the server where it was.
// An address that names a line marks it: the page opens with it in view and Follow off, and it stays
// marked while its own page is shown unfiltered. Opened plainly, the tab starts at `preferred` (the
// latest failure's log) when the run has it, else at the first file.
function LogsPanel({ files, token, onSignedOut, reloads, base, wanted, cursor, mark = null, runId, preferred = null }) {
  const openable = files.filter((file) => LOG_FILE.test(file.name));
  const first = wanted !== null ? wanted : openable.some((file) => file.name === preferred) ? preferred : openable.length > 0 ? openable[0].name : null;
  const [name, setName] = useState(first);
  const [pages, setPages] = useState([wanted !== null ? cursor : null]);
  const [text, setText] = useState("");
  const [level, setLevel] = useState("all");
  const [follow, setFollow] = useState(mark === null);
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
  // The marked line's place in the page the address opened, which starts after `cursor` lines.
  const marked = mark !== null && wanted !== null && name === wanted && pages.length === 1 ? mark - 1 - (cursor === null ? 0 : Number(cursor)) : null;
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
      {name === null ? <p>No logs for this run.</p> : <Loaded key={pagePath} reloads={reloads} token={token} path={pagePath} loading="Loading the log…" parse={parseLogPage} draw={(page) => <LogPage page={page} filters={filters} follow={follow} onFollow={setFollow} pager={pager} marked={marked} />} onSignedOut={onSignedOut} />}
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

function LogLine({ line, marked = false }) {
  const time = LOG_TIME.exec(line);
  return (
    <div className="log-line" data-level={lineLevel(line)} id={marked ? "log-mark" : undefined} data-log-mark={marked ? "" : undefined}>
      {time === null ? line : <><span className="log-time">{time[1]}</span>{line.slice(time[1].length)}</>}
    </div>
  );
}

// The lines, which follow new ones while Follow is on. Scrolling up turns it off, and the lines that
// arrived since are counted beside a control that jumps to the latest; reaching the bottom turns it on.
function LogLines({ shown, total, follow = true, onFollow = () => {}, marked = null }) {
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
      // A marked line is brought into view as the page opens.
      const mark = marked === null ? null : el.querySelector("[data-log-mark]");
      if (mark !== null && typeof mark.scrollIntoView === "function") mark.scrollIntoView({ block: "center" });
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
          <LogLine key={at} line={line} marked={at === marked} />
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
function LogPage({ page, filters: { text, setText, level, setLevel }, follow, onFollow, pager: { number, onPrev, onNext }, marked = null }) {
  const { lines, next_cursor: next, truncated } = page;
  const shown = filterLines(lines, text, level);
  // A filtered list has other lines at each place, so the mark is only drawn on the whole page.
  const mark = marked !== null && text === "" && level === "all" && marked >= 0 && marked < lines.length ? marked : null;
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
      <LogLines shown={shown} total={lines.length} follow={follow} onFollow={onFollow} marked={mark} />
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
const DELIVERIES = [["review", "Open review"], ["merge_local", "Merge locally"]];

// A record saved before push was removed still names it: the page says so and chooses nothing for it.
const PUSH_REMOVED = "the push delivery was removed: use review (pushes and opens a review) or merge-local";

function isPicked(agent) {
  return agent !== null && typeof agent === "object" && typeof agent.harness === "string" && typeof agent.model === "string" && (agent.effort === null || agent.effort === undefined || typeof agent.effort === "string");
}

function parseProjectSettings(data) {
  const settings = data && data.settings;
  const role = (entry) => entry !== null && typeof entry === "object" && (entry.project === null || isPicked(entry.project)) && (entry.fallback === null || isPicked(entry.fallback));
  const limit = (value, range) => value === undefined || value === null || (Number.isInteger(value) && value >= range[0] && value <= range[1]);
  const ok = settings && typeof settings === "object" && role(settings.implementer) && role(settings.reviewer) && (settings.delivery === "push" || DELIVERIES.some(([value]) => value === settings.delivery)) && Number.isInteger(settings.revision) && settings.revision >= 0
    && limit(settings.max_active_runs, ACTIVE_RUNS_RANGE) && limit(settings.min_free_disk_gb, FREE_DISK_RANGE)
    && (settings.queue_hours === undefined || settings.queue_hours === null || typeof settings.queue_hours === "string")
    && (settings.queue_stop_on_failure === undefined || typeof settings.queue_stop_on_failure === "boolean")
    && (settings.queue_wait_for_review === undefined || typeof settings.queue_wait_for_review === "boolean")
    && (settings.pull_target_before_runs === undefined || typeof settings.pull_target_before_runs === "boolean")
    && (settings.after_merge === undefined || settings.after_merge === "ask" || settings.after_merge === "auto")
    && (settings.target_branch === undefined || typeof settings.target_branch === "string")
    && DESIGN_ROLES.every(([, role]) => settings[role] === undefined || settings[role] === null || isPicked(settings[role]));
  // Waiting for each run's review, and pulling the target, are on unless the server says they are off, as their defaults are.
  const target = typeof settings?.target_branch === "string" && settings.target_branch !== "" ? settings.target_branch : "the target";
  return ok ? { ...settings, max_active_runs: settings.max_active_runs ?? null, min_free_disk_gb: settings.min_free_disk_gb ?? null, queue_hours: settings.queue_hours ?? null, queue_stop_on_failure: settings.queue_stop_on_failure === true, queue_wait_for_review: settings.queue_wait_for_review !== false, pull_target_before_runs: settings.pull_target_before_runs !== false, after_merge: settings.after_merge === "auto" ? "auto" : "ask", target_branch: target } : null;
}

// The limits a project may put on starting a run, as the server checks them; empty is no limit.
const ACTIVE_RUNS_RANGE = [1, 8];
const FREE_DISK_RANGE = [1, 500];

function activeRunsLine(max) {
  return max === null ? "No limit" : "At most " + max + (max === 1 ? " active run" : " active runs");
}

// Typed queue hours as Save sends them: null for empty, "HH:MM-HH:MM" as typed, or undefined when they are
// not a window the server reads (two different times of day), which holds Save back.
function typedHours(text) {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const match = /^([01][0-9]|2[0-3]):([0-5][0-9])-([01][0-9]|2[0-3]):([0-5][0-9])$/.exec(trimmed);
  return match !== null && (match[1] !== match[3] || match[2] !== match[4]) ? trimmed : undefined;
}

function freeDiskLine(gb) {
  return gb === null ? "No limit" : gb + " GB";
}

// A typed free-disk floor as Save sends it: null for empty, the whole number of GB, or undefined when it is
// neither, which holds Save back.
function typedFloor(text) {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  if (!/^[0-9]+$/.test(trimmed)) return undefined;
  const gb = Number(trimmed);
  return gb >= FREE_DISK_RANGE[0] && gb <= FREE_DISK_RANGE[1] ? gb : undefined;
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
  const offered = DELIVERIES.find(([value]) => value === delivery);
  return offered === undefined ? PUSH_REMOVED : offered[1];
}

function isOfferedDelivery(delivery) {
  return DELIVERIES.some(([value]) => value === delivery);
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
// A design role (`unset` and `reset` name what it is without one) reuses it with no machine default.
function RoleFields({ title, role, entry, options, choice, disabled, onChoose, unset, reset = "Use machine default", className = "settings-block project-role-fields" }) {
  const current = choice === null ? null : options.find((option) => option.harness === choice.harness);
  const levels = effortsOf(current);
  const harnesses = options.map((option) => option.harness);
  const pickHarness = (harness) => {
    const option = options.find((each) => each.harness === harness);
    onChoose(option ? { harness, model: option.models[0], effort: keptEffort(option, choice && choice.effort) } : null);
  };
  const machine = unset !== undefined ? unset : entry.fallback === null ? "Machine default" : "Machine default (" + agentLine(entry.fallback) + ")";
  return (
    <div className={className}>
      <select data-role-harness={role} aria-label={title + " harness"} value={choice === null ? "" : choice.harness} disabled={disabled} onChange={(event) => pickHarness(event.target.value)}>
        <option value="">{machine}</option>
        {(choice === null ? harnesses.map((harness) => [harness, harness]) : withCurrent(harnesses, choice.harness)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select>
      {choice !== null ? <select data-role-model={role} aria-label={title + " model"} value={choice.model} disabled={disabled} onChange={(event) => onChoose({ ...choice, model: event.target.value })}>{withCurrent(current ? current.models : [], choice.model).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select> : null}
      {current && levels.length > 0 ? <select data-role-effort={role} aria-label={title + " effort"} value={choice.effort} disabled={disabled} onChange={(event) => onChoose({ ...choice, effort: event.target.value })}><option value="">Not set</option>{(choice.effort === "" ? levels.map((level) => [level, level]) : withCurrent(levels, choice.effort)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select> : null}
      <button type="button" data-role-reset={role} disabled={disabled || choice === null} onClick={() => onChoose(null)}>{reset}</button>
    </div>
  );
}

// One setting of a project settings card ("Starting runs", "Queue") as a settings row: its label on the left and its value on the
// right, wrapping under the label on a phone; while editing, the label wraps the control (its accessible
// name) and the control with its message stacks under or beside it.
function SettingsRow({ label, editing, value, children }) {
  if (!editing) {
    return (
      <div className="settings-row" data-setting-row>
        <span className="settings-label" data-setting-label>{label}</span>
        <span className="settings-row-value" data-setting-value>{value}</span>
      </div>
    );
  }
  return (
    <label className="settings-row settings-field" data-setting-row>
      <span className="settings-label" data-setting-label>{label}</span>
      <span className="settings-field-control">{children}</span>
    </label>
  );
}

const PROJECT_ROLES = [["Implementer", "implementer"], ["Reviewer", "reviewer"]];

// The frontend design roles: each one's title, key, what it is while unset, and a model to suggest, which is
// only ever shown as a hint and never chosen for the project.
const DESIGN_ROLES = [
  ["Design preparer", "design_preparer", "Uses the design reviewer", "pi openai/gpt-6.1-sol, thinking medium"],
  ["Design reviewer", "design_reviewer", "Not set: a frontend run needs one", "pi openai/gpt-6.1-terra, thinking medium"],
  ["Design verifier", "design_verifier", "Uses the design reviewer at thinking high", "pi openai/gpt-6.1-sol, thinking high"],
];
// The harnesses Orq runs read-only for review, the only ones a design role may use.
const DESIGN_HARNESSES = ["pi", "claude"];
const DESIGN_HELP = "Used only by frontend features. The implementer and code reviewer above build and review the code.";

// The design roles as a settings card: a row per role, its agent or what it is while unset; while editing, the
// role's harness (pi or claude), model and thinking, with a suggestion under a role left unset.
function DesignAgentsCard({ settings, editing, draft, options, disabled, onChoose }) {
  const offered = options.filter((option) => DESIGN_HARNESSES.includes(option.harness));
  return (
    <section className="card settings-card" data-design-agents>
      <SettingsHead icon="cpu">Frontend design</SettingsHead>
      <p className="settings-hint muted" data-design-help>{DESIGN_HELP}</p>
      {DESIGN_ROLES.map(([title, role, unset, suggestion]) => editing ? (
        <div key={role} className="settings-row" data-setting-row data-design-role={role}>
          <span className="settings-label" data-setting-label>{title}</span>
          <span className="settings-field-control">
            <RoleFields title={title} role={role} entry={{ fallback: null }} options={offered} choice={draft[role]} disabled={disabled} onChoose={(choice) => onChoose(role, choice)} unset={unset} reset="Unset" className="project-role-fields" />
            {draft[role] === null ? <span className="muted" data-design-suggestion>{"Suggested: " + suggestion}</span> : null}
          </span>
        </div>
      ) : (
        <SettingsRow key={role} label={title} editing={false} value={<span data-project-design-role={role}>{settings[role] === null ? unset : agentLine(settings[role])}</span>} />
      ))}
    </section>
  );
}

// A project's settings: each role and the delivery, read only until Edit. Edit checks that remote actions
// are on and reads the machine's options; Save asks, then sends the whole record with the revision Edit
// read, and a refusal shows the server's reason, with Reload when the settings changed meanwhile.
function ProjectSettingsView({ settings, path, token, projectId, onSignedOut, onChanged }) {
  const [phase, setPhase] = useState("idle");
  const [options, setOptions] = useState([]);
  const [draft, setDraft] = useState(null);
  const [note, setNote] = useState("");
  const [conflict, setConflict] = useState(false);
  // A server older than the design roles sends none, and a save then leaves them out too.
  const hasDesign = settings.design_reviewer !== undefined;
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
      const design = hasDesign ? Object.fromEntries(DESIGN_ROLES.map(([, role]) => [role, startingRole(settings[role])])) : {};
      setDraft({ ...design, implementer: startingRole(settings.implementer.project), reviewer: startingRole(settings.reviewer.project), delivery: isOfferedDelivery(settings.delivery) ? settings.delivery : "", maxActiveRuns: settings.max_active_runs === null ? "" : String(settings.max_active_runs), minFreeDisk: settings.min_free_disk_gb === null ? "" : String(settings.min_free_disk_gb), queueHours: settings.queue_hours ?? "", stopOnFailure: settings.queue_stop_on_failure, waitForReview: settings.queue_wait_for_review, pullTarget: settings.pull_target_before_runs, afterMerge: settings.after_merge, revision: settings.revision });
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
      const design = hasDesign ? Object.fromEntries(DESIGN_ROLES.map(([, role]) => [role, sentRole(draft[role])])) : {};
      const response = await actionRequest(path, token, { ...design, implementer: sentRole(draft.implementer), reviewer: sentRole(draft.reviewer), delivery: draft.delivery, max_active_runs: draft.maxActiveRuns === "" ? null : Number(draft.maxActiveRuns), min_free_disk_gb: typedFloor(draft.minFreeDisk), queue_hours: typedHours(draft.queueHours), queue_stop_on_failure: draft.stopOnFailure, queue_wait_for_review: draft.waitForReview, pull_target_before_runs: draft.pullTarget, after_merge: draft.afterMerge, revision: draft.revision });
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
      {hasDesign ? <DesignAgentsCard settings={settings} editing={editing} draft={draft} options={options} disabled={phase !== "editing"} onChoose={(role, choice) => setDraft({ ...draft, [role]: choice })} /> : null}
      <section className="card settings-card">
        <SettingsHead icon="flag">When a run finishes</SettingsHead>
        <div className="settings-block">
          {editing ? (
            <select data-delivery aria-label="When a run finishes" value={draft.delivery} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, delivery: event.target.value })}>
              {isOfferedDelivery(draft.delivery) ? null : <option value="" disabled>Choose one</option>}
              {DELIVERIES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
          ) : (
            <span data-project-delivery>{deliveryLabel(settings.delivery)}</span>
          )}
        </div>
      </section>
      <section className="card settings-card" data-starting-runs>
        <SettingsHead icon="play">Starting runs</SettingsHead>
        <SettingsRow label="At most N active runs" editing={editing} value={<span data-project-max-active-runs>{activeRunsLine(settings.max_active_runs)}</span>}>
          {editing ? <select data-max-active-runs aria-label="At most N active runs" value={draft.maxActiveRuns} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, maxActiveRuns: event.target.value })}>
            <option value="">No limit</option>
            {Array.from({ length: ACTIVE_RUNS_RANGE[1] - ACTIVE_RUNS_RANGE[0] + 1 }, (_, index) => String(ACTIVE_RUNS_RANGE[0] + index)).map((value) => <option key={value} value={value}>{value}</option>)}
          </select> : null}
        </SettingsRow>
        <SettingsRow label="Free disk needed to start (GB)" editing={editing} value={<span data-project-min-free-disk>{freeDiskLine(settings.min_free_disk_gb)}</span>}>
          {editing ? <input type="text" inputMode="numeric" data-min-free-disk aria-label="Free disk needed to start (GB)" placeholder="No limit" value={draft.minFreeDisk} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, minFreeDisk: event.target.value })} /> : null}
          {editing && typedFloor(draft.minFreeDisk) === undefined ? <span className="error-text" data-min-free-disk-invalid>Leave it empty for no limit, or give a whole number from 1 to 500.</span> : null}
        </SettingsRow>
      </section>
      <section className="card settings-card" data-queue-settings>
        <SettingsHead icon="clock">Queue</SettingsHead>
        <SettingsRow label="Hours the queue starts runs" editing={editing} value={<span data-project-queue-hours>{settings.queue_hours === null ? "Any time" : settings.queue_hours}</span>}>
          {editing ? <input type="text" data-queue-hours aria-label="Hours the queue starts runs" placeholder="Any time, or 22:00-07:00" value={draft.queueHours} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, queueHours: event.target.value })} /> : null}
          {editing && typedHours(draft.queueHours) === undefined ? <span className="error-text" data-queue-hours-invalid>Leave it empty for any time, or give a window like 22:00-07:00.</span> : null}
        </SettingsRow>
        {editing ? (
          <label className="settings-row settings-check">
            <input type="checkbox" data-queue-stop-on-failure checked={draft.stopOnFailure} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, stopOnFailure: event.target.checked })} />
            <span>A failed run pauses the queue</span>
          </label>
        ) : (
          <SettingsRow label="When a run fails" editing={false} value={<span data-project-queue-stop>{settings.queue_stop_on_failure ? "Pauses the queue" : "Keeps going; only its dependents wait"}</span>} />
        )}
        {editing ? (
          <label className="settings-row settings-check">
            <input type="checkbox" data-queue-wait-for-review checked={draft.waitForReview} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, waitForReview: event.target.checked })} />
            <span>Wait for each run's review (and MR) before the next</span>
          </label>
        ) : (
          <SettingsRow label="Wait for each run's review (and MR) before the next" editing={false} value={<span data-project-queue-wait>{settings.queue_wait_for_review ? "Waits until it is merged, accepted or discarded" : "Starts the next as the run limit allows"}</span>} />
        )}
        {editing ? (
          <label className="settings-row settings-check">
            <input type="checkbox" data-pull-target-before-runs checked={draft.pullTarget} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, pullTarget: event.target.checked })} />
            <span>{"Pull " + settings.target_branch + " after an MR merges and before each run"}</span>
          </label>
        ) : (
          <SettingsRow label={"Pull " + settings.target_branch + " after an MR merges and before each run"} editing={false} value={<span data-project-pull-target>{settings.pull_target_before_runs ? "Fast-forwards " + settings.target_branch + " first" : "Leaves " + settings.target_branch + " as it is"}</span>} />
        )}
        {editing ? (
          <label className="settings-row settings-check">
            <input type="checkbox" data-after-merge checked={draft.afterMerge === "auto"} disabled={phase !== "editing"} onChange={(event) => setDraft({ ...draft, afterMerge: event.target.checked ? "auto" : "ask" })} />
            <span>After an MR merges, clean up on its own (mark the feature completed, delete the worktree and branch, fast-forward {settings.target_branch})</span>
          </label>
        ) : (
          <SettingsRow label="After an MR merges" editing={false} value={<span data-project-after-merge>{settings.after_merge === "auto" ? "Cleans up on its own and tells you" : "Offers Clean up on the run"}</span>} />
        )}
      </section>
      <ProjectNotificationsCard token={token} projectId={projectId} />
      {phase === "editing" ? (
        <div className="settings-block settings-actions">
          <button type="button" className="primary-button" data-project-settings-save disabled={!isOfferedDelivery(draft.delivery) || typedFloor(draft.minFreeDisk) === undefined || typedHours(draft.queueHours) === undefined} onClick={() => setPhase("asking")}><Icon name="check" size={16} />Save</button>
          <button type="button" data-cancel onClick={() => { setPhase("idle"); setNote(""); setConflict(false); }}>Cancel</button>
        </div>
      ) : null}
      {phase === "asking" ? (
        <Confirm question="Save this project's settings?" detail="Runs already going keep their agents and delivery, and are not stopped by a new limit." answer="Yes, save" tone="primary" onConfirm={send} onCancel={() => setPhase("editing")} />
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
      )} token={token} path={settingsPath} loading="Loading the project's settings…" parse={parseProjectSettings} draw={(settings) => <ProjectSettingsView settings={settings} path={settingsPath} token={token} projectId={projectId} onSignedOut={onSignedOut} onChanged={onRefresh} />} onSignedOut={onSignedOut} />
    );
  }
  if (/^\/projects\/[^/]+\/queue$/.test(address)) {
    const queuePath = "/api/projects/" + encodeURIComponent(projectId) + "/queue";
    return (
      <Loaded key={queuePath} reloads={reloads} header={() => (
        <>
          <PageHeader title="Queue" subtitle="The features the server starts in turn, when the project's limits allow" onRefresh={onRefresh} />
          <ProjectSelect projects={projects} projectId={projectId} section="queue" />
        </>
      )} token={token} path={queuePath} loading="Loading the queue…" parse={parseQueue} draw={(data) => <QueueView key={JSON.stringify(data.queue)} data={data} projectId={projectId} control={data.actions ? { token, projectId, onSignedOut, onDone: onRefresh } : null} push={{ token, projectId, onSignedOut }} />} onSignedOut={onSignedOut} />
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
  const logs = /^\/projects\/[^/]+\/orchestrations\/([^/]+)\/logs(?:\/([^/]+)(?:\/after\/([^/]+)(?:\/line\/([^/]+))?)?)?$/.exec(address);
  const timeline = logs !== null ? null : /^\/projects\/[^/]+\/orchestrations\/([^/]+)(?:\/after\/([^/]+))?$/.exec(address);
  if (logs !== null || timeline !== null) {
    let run = null;
    let cursor = null;
    let file = null;
    let line = null;
    let mark = null;
    try {
      if (logs !== null) {
        run = decodeURIComponent(logs[1]);
        file = logs[2] === undefined ? null : decodeURIComponent(logs[2]);
        line = logs[3] === undefined ? null : decodeURIComponent(logs[3]);
        mark = logs[4] === undefined ? null : decodeURIComponent(logs[4]);
      } else {
        run = decodeURIComponent(timeline[1]);
        cursor = timeline[2] === undefined ? null : decodeURIComponent(timeline[2]);
      }
    } catch (malformed) {
      return <NotFound />;
    }
    if (!RUN_ID.test(run) || (cursor !== null && !RUN_ID.test(cursor)) || (file !== null && !LOG_FILE.test(file)) || (line !== null && !LINE_CURSOR.test(line)) || (mark !== null && !MARKED_LINE.test(mark))) return <NotFound />;
    const listPath = "/projects/" + encodeURIComponent(projectId) + "/orchestrations";
    const runPath = listPath + "/" + encodeURIComponent(run);
    const apiRun = "/api/projects/" + encodeURIComponent(projectId) + "/runs/" + encodeURIComponent(run);
    const timelinePath = apiRun + (cursor === null ? "" : "/after/" + encodeURIComponent(cursor));
    const logsAt = { token, onSignedOut, reloads, base: apiRun + "/logs", file, cursor: line, mark: mark === null ? null : Number(mark) };
    return (
      <>
        <Loaded key={timelinePath} reloads={reloads} header={(state) => (
          <>
            <PageHeader title="Run" subtitle={run} onRefresh={onRefresh} />
            <ProjectSelect projects={projects} projectId={projectId} section="orchestrations" />
          </>
        )} token={token} path={timelinePath} loading="Loading the run…" parse={parseTimeline} draw={(data) => <RunTimeline key={address} timeline={data} listPath={listPath} runPath={runPath} startTab={logs !== null ? "logs" : "timeline"} projectId={projectId} logs={logsAt} follow={{ token, onSignedOut, cursor, base: apiRun }} control={data.remote_actions === true ? { token, projectId, onSignedOut, onDone: onRefresh } : null} />} onSignedOut={onSignedOut} />
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
          <Loaded key={API_SETTINGS} reloads={reloads} token={token} path={API_SETTINGS} loading="Loading the settings…" parse={parseSettings} draw={(settings) => <SettingsView settings={settings} reloads={reloads} token={token} onSignedOut={onSignedOut} onChanged={() => setReloads(reloads + 1)} />} onSignedOut={onSignedOut} />
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
        <Loaded key={API_PROJECTS} reloads={reloads} header={(state) => <PageHeader title="Dashboard" subtitle="Monitor Orq projects and orchestration activity" onRefresh={() => setReloads(reloads + 1)} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseDashboard} draw={({ projects, machine }) => <Dashboard projects={projects} machine={machine} token={token} onSignedOut={onSignedOut} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  if (hash === "/todos") {
    return (
      <Loaded key={API_PROJECTS} reloads={reloads} header={() => <PageHeader title="Todos" subtitle="Ideas waiting for a project" onRefresh={() => setReloads(reloads + 1)} />} token={token} path={API_PROJECTS} loading="Loading the projects…" parse={parseDashboard} draw={({ projects }) => <TodosPage projects={projects} reloads={reloads} token={token} onSignedOut={onSignedOut} />} onSignedOut={onSignedOut} />
    );
  }
  if (hash === "/features") {
    return <AllFeaturesPage token={token} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} onSignedOut={onSignedOut} />;
  }
  if (hash === "/queue") {
    return <AllQueuesPage token={token} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} onSignedOut={onSignedOut} />;
  }
  if (hash === "/orchestrations") {
    return <AllOrchestrationsPage token={token} reloads={reloads} onRefresh={() => setReloads(reloads + 1)} onSignedOut={onSignedOut} />;
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

// How often the sign-in page asks again while the server cannot be reached.
const RETRY_MS = 15000;

function App() {
  const [token, setTokenState] = useState(keptToken);
  const [status, setStatus] = useState(null);
  const setToken = (next) => {
    keepToken(next);
    setTokenState(next);
  };
  const [message, setMessage] = useState("");
  // Only the answer to the latest request counts: an older one that ends late (a timeout after a newer
  // request already got through) would otherwise undo it.
  const latest = useRef(0);
  const refresh = () => {
    latest.current += 1;
    const mine = latest.current;
    return authStatus().then((next) => {
      if (mine === latest.current) setStatus(next);
    });
  };
  useEffect(() => {
    refresh();
  }, []);
  // While nobody is signed in and the server cannot be reached, try again once the device is back online,
  // when the page is shown again, and every RETRY_MS while it stays visible.
  const waiting = status === UNREACHABLE && token === null;
  useEffect(() => {
    if (!waiting) return undefined;
    const visible = () => document.visibilityState === "visible";
    const shown = () => visible() && refresh();
    const timer = setInterval(shown, RETRY_MS);
    window.addEventListener("online", refresh);
    document.addEventListener("visibilitychange", shown);
    return () => {
      clearInterval(timer);
      window.removeEventListener("online", refresh);
      document.removeEventListener("visibilitychange", shown);
    };
  }, [waiting]);
  // The server's answer when it gave one; null while unknown, without passkey support, or unreachable.
  const reply = status !== null && typeof status === "object" ? status : null;
  const session = reply !== null && reply.signed_in === true;
  const active = session && token === null ? SESSION : token;
  if (active === null) {
    return <main><TokenPrompt message={message} onToken={setToken} passkeys={reply !== null && reply.passkeys} known={reply !== null} onPasskey={refresh} blocker={reply !== null ? passkeyBlocker() : null} unreachable={status === UNREACHABLE} onRetry={refresh} /></main>;
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
  const first = typeof active === "string" && reply !== null && reply.passkeys === false && passkeysAvailable();
  const blocker = reply !== null ? passkeyBlocker() : null;
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
// A new release is offered, never forced: the page reloads into it only when the person asks.
// It floats over the page, taking no place in its layout: a scrim and a modal card holding Reload.
function UpdateBanner() {
  const [waiting, setWaiting] = useState(null);
  const [open, setOpen] = useState(true);
  const reload = useRef(null);
  const fab = useRef(null);
  const card = useRef(null);
  const before = useRef(null);
  useEffect(() => startWorker(window, setWaiting), []);
  useEffect(() => {
    if (waiting === null) return;
    if (open) {
      const at = document.activeElement;
      before.current = at !== null && at !== document.body ? at : null;
      if (reload.current !== null) reload.current.focus();
      const onKey = (e) => {
        if (e.key === "Escape") setOpen(false);
        if (e.key !== "Tab" || card.current === null) return;
        const buttons = card.current.querySelectorAll("button");
        const first = buttons[0];
        const last = buttons[buttons.length - 1];
        const at = document.activeElement;
        if (at === (e.shiftKey ? first : last) || !card.current.contains(at)) {
          e.preventDefault();
          (e.shiftKey ? last : first).focus();
        }
      };
      document.addEventListener("keydown", onKey);
      return () => document.removeEventListener("keydown", onKey);
    }
    const back = before.current;
    if (back !== null && back.isConnected) back.focus();
    else if (fab.current !== null) fab.current.focus();
  }, [waiting, open]);
  if (waiting === null) return null;
  if (!open) {
    return (
      <button type="button" className="update-fab" ref={fab} data-update-fab aria-label="Update available: reload" onClick={() => setOpen(true)}>
        <Icon name="refresh" />
        <span className="update-dot" />
      </button>
    );
  }
  return (
    <>
      <p className="sr-only" aria-live="polite">Update available</p>
      <div className="update-scrim" onClick={() => setOpen(false)} />
      <div className="update-card" ref={card} role="dialog" aria-modal="true" aria-labelledby="update-title">
        <Icon name="refresh" />
        <div className="update-text">
          <strong id="update-title">Update available</strong>
          <span className="muted">A new version of orq is ready.</span>
        </div>
        <div className="update-buttons">
          <button type="button" className="update-later" data-update-later onClick={() => setOpen(false)}>Later</button>
          <button type="button" className="primary-button" ref={reload} data-update-reload onClick={() => applyUpdate(window, waiting)}>Reload</button>
        </div>
      </div>
    </>
  );
}

createRoot(document.getElementById("app")).render(<><UpdateBanner /><App /></>);
