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
import { currentPath, migrateLegacyHash, navigate, subscribeToLocation } from "./router.js";

const API_REVIEWS = "/api/reviews";
// One message for every failed sign-in, so it never says which part failed.
const SIGN_IN_FAILED = "Could not sign in. Check the token and try again.";
const STALE_MARKER = "stale, may be out of date";
const DETAIL_PREFIX = "/reviews/";
// Places the sidebar offers that have no page yet; they say so instead of showing the inbox.
const UNBUILT_DESTINATIONS = ["/settings", "/projects"];
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

function Generated({ at }) {
  if (typeof at !== "string" || !UTC_TIME.test(at)) return null;
  const when = new Date(at);
  if (Number.isNaN(when.getTime())) return null;
  return (
    <span className="generated">
      Generated <time dateTime={at}>{when.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}</time>
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

function TokenPrompt({ message, onToken }) {
  const field = useRef(null);
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
      <p>Sign in with token: paste the access token printed by orq serve.</p>
      <label>
        Access token
        <input ref={field} type="password" autoComplete="off" spellCheck={false} aria-label="Access token" onKeyDown={(event) => event.key === "Enter" && submit()} />
      </label>
      <button type="button" onClick={submit}>
        Open the inbox
      </button>
      <p role="alert">{message}</p>
      <div className="alternatives" />
      <ThemeToggle />
    </div>
  );
}

function RunRow({ run, detailPrefix }) {
  const open = typeof run.run_id === "string" && RUN_ID.test(run.run_id);
  return (
    <li className={run.stale === true ? "run stale card" : "run card"}>
      {open ? <span className="ticket-id">{run.run_id}</span> : null}
      {open ? (
        <button type="button" onClick={() => navigate(detailPrefix + run.run_id)}>
          {String(run.feature)}
        </button>
      ) : (
        <strong>{String(run.feature)}</strong>
      )}
      <span>{String(run.status)}</span>
      {run.risk !== undefined && run.risk !== null && <span>{"risk: " + String(run.risk)}</span>}
      <Generated at={run.generated_at} />
      {run.stale === true && <em>{STALE_MARKER}</em>}
    </li>
  );
}

function RunList({ runs, detailPrefix = DETAIL_PREFIX }) {
  if (runs.length === 0) {
    return <p>No reviews in the inbox.</p>;
  }
  return (
    <ul>
      {runs.map((run, index) => (
        <RunRow key={index} run={run} detailPrefix={detailPrefix} />
      ))}
    </ul>
  );
}

function Detail({ report, listPath = "/" }) {
  const href = report.stale ? null : safeHttpsLink(report.link);
  return (
    <>
      <button type="button" onClick={() => navigate(listPath)}>
        Back to the list
      </button>
      <Generated at={report.generated_at} />
      {report.stale && <p>{STALE_MARKER}</p>}
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
function get(path, token, signal) {
  return fetch(path, { method: "GET", headers: { Authorization: "Bearer " + token }, cache: "no-store", referrerPolicy: "no-referrer", signal });
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
  return new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
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
          Last checked {state.checked}
        </p>
      </>
    );
  }
  return (
    <>
      <div className="banner" role="alert">
        {state.kind === "disconnected" ? <p>Disconnected. Last checked {state.checked}</p> : <p>The inbox could not be read just now.</p>}
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

const FEATURE_GROUPS = ["draft", "pending", "in_progress", "completed", "abandoned"];

function isFeature(feature) {
  return feature !== null && typeof feature === "object" && typeof feature.name === "string" && typeof feature.status === "string" && (feature.description === null || typeof feature.description === "string") && isTextList(feature.depends_on);
}

function parseFeatures(data) {
  return data && Array.isArray(data.features) && data.features.every(isFeature) ? data.features : null;
}

// Features grouped by status in the order work moves through, a status the page does not know last.
function FeatureList({ features }) {
  if (features.length === 0) {
    return <p>No features in this project's ledger.</p>;
  }
  const known = new Set(features.map((feature) => feature.name));
  const statuses = [...FEATURE_GROUPS.filter((status) => features.some((feature) => feature.status === status)), ...[...new Set(features.map((feature) => feature.status))].filter((status) => !FEATURE_GROUPS.includes(status))];
  return (
    <>
      {statuses.map((status) => (
        <section key={status} className="feature-group">
          <h2 className="feature-status">{status}</h2>
          <ul>
            {features
              .filter((feature) => feature.status === status)
              .map((feature) => (
                <li key={feature.name} className="feature card">
                  <strong>{feature.name}</strong>
                  {feature.description ? <p>{feature.description}</p> : null}
                  {feature.depends_on.length > 0 ? <p className="depends-on">{"Depends on: " + feature.depends_on.map((name) => (known.has(name) ? name : name + " (not in this ledger)")).join(", ")}</p> : null}
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
            {run.updated_at ? <p>{"Last activity " + run.updated_at}</p> : null}
            {run.started_at ? <p>{"Started " + run.started_at}</p> : null}
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
      {run.started_at ? <p>{"Started " + run.started_at}</p> : null}
      {run.updated_at ? <p>{"Last activity " + run.updated_at}</p> : null}
      <button type="button" data-open="logs" onClick={() => navigate(runPath + "/logs")}>View logs</button>
      {events.length === 0 ? <p>No events recorded.</p> : null}
      <ol>
        {events.map((event) => (
          <li key={event.event_id} className="event card">
            <strong>{event.kind}</strong>
            {event.stage ? <span>{"Stage: " + event.stage}</span> : null}
            <span>{"Attempt " + event.attempt}</span>
            {event.occurred_at ? <time>{event.occurred_at}</time> : null}
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
          if (afterCr && text.startsWith("\n")) text = text.slice(1);
          if (text.length > 0) afterCr = text.endsWith("\r");
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
            {event.occurred_at ? <time>{event.occurred_at}</time> : null}
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

// One page of a log's already-redacted lines, drawn as plain text. The next page is its own address.
function LogPage({ page, logsPath, filePath }) {
  const { lines, next_cursor: next, truncated } = page;
  return (
    <>
      <button type="button" data-back="logs" onClick={() => navigate(logsPath)}>Back to the logs</button>
      <h2>{page.name}</h2>
      {lines.length === 0 ? <p>This log has no more lines.</p> : null}
      <pre className="log-lines">
        {lines.map((line, at) => (
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
        <Loaded key={featuresPath} reloads={reloads} token={token} path={featuresPath} loading="Loading the features…" parse={parseFeatures} draw={(features) => <FeatureList features={features} />} onSignedOut={onSignedOut} />
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
        <Loaded key={pagePath} reloads={reloads} token={token} path={pagePath} loading="Loading the log…" parse={parseLogPage} draw={(page) => <LogPage page={page} logsPath={logsPath} filePath={filePath} />} onSignedOut={onSignedOut} />
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
  if (match[1] === undefined) {
    return (
      <>
        <PageHeader title="Reviews" subtitle="Code reviews and agent reports" onRefresh={onRefresh} />
        <Loaded key={base} reloads={reloads} token={token} path={base} loading="Loading the review inbox…" parse={parseRuns} draw={(runs) => <RunList runs={runs} detailPrefix={listPath + "/"} />} onSignedOut={onSignedOut} />
      </>
    );
  }
  if (!RUN_ID.test(match[1])) return <NotFound />;
  const path = base + "/" + encodeURIComponent(match[1]);
  return <Loaded key={path} token={token} path={path} loading="Loading the report…" parse={parseDetail} draw={(report) => <Detail report={report} listPath={listPath} />} onSignedOut={onSignedOut} />;
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

function App() {
  const [token, setTokenState] = useState(keptToken);
  const setToken = (next) => {
    keepToken(next);
    setTokenState(next);
  };
  const [message, setMessage] = useState("");
  if (token === null) {
    return <TokenPrompt message={message} onToken={setToken} />;
  }
  const signedOut = () => {
    setToken(null);
    setMessage(SIGN_IN_FAILED);
  };
  return (
    <Scoped token={token} onSignedOut={signedOut} onSignOut={() => { setMessage(""); setToken(null); }} />
  );
}

migrateLegacyHash();
createRoot(document.getElementById("app")).render(<App />);
