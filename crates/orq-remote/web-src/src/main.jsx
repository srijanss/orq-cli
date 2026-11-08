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

function RunRow({ run }) {
  const open = typeof run.run_id === "string" && RUN_ID.test(run.run_id);
  return (
    <li className={run.stale === true ? "run stale card" : "run card"}>
      {open ? <span className="ticket-id">{run.run_id}</span> : null}
      {open ? (
        <button type="button" onClick={() => navigate(DETAIL_PREFIX + run.run_id)}>
          {String(run.feature)}
        </button>
      ) : (
        <strong>{String(run.feature)}</strong>
      )}
      <span>{String(run.status)}</span>
      {run.risk !== undefined && run.risk !== null && <span>{"risk: " + String(run.risk)}</span>}
      {run.stale === true && <em>{STALE_MARKER}</em>}
    </li>
  );
}

function RunList({ runs }) {
  if (runs.length === 0) {
    return <p>No reviews in the inbox.</p>;
  }
  return (
    <ul>
      {runs.map((run, index) => (
        <RunRow key={index} run={run} />
      ))}
    </ul>
  );
}

function Detail({ report }) {
  const href = report.stale ? null : safeHttpsLink(report.link);
  return (
    <>
      <button type="button" onClick={() => navigate("/")}>
        Back to the list
      </button>
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

function checkedTime() {
  return new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function Loaded({ token, path, loading, parse, draw, onSignedOut, reloads = 0 }) {
  const [state, setState] = useState({ kind: "loading", data: null });
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let current = true;
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
        else setState({ kind: "ready", data });
      } catch (failure) {
        if (current) fail("disconnected");
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
    return draw(state.data);
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

function SignedIn({ token, onSignedOut, projectId = null, projects = null }) {
  const [reloads, setReloads] = useState(0);
  const hash = useSyncExternalStore(subscribeToLocation, currentPath);
  if (projectId !== null && projects !== null && !projects.some((project) => project.id === projectId)) {
    return <NotFound />;
  }
  if (projectId !== null) {
    return <p>This page is not available yet.</p>;
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
