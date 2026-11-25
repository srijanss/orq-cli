import { Icon } from "./icons.jsx";

// Every state, risk and freshness label is one status pill: its icon, then its words, in its tone. The
// tone names the token pair it is drawn in (`--<tone>` on `--<tone>-bg`), or "neutral" for a muted one;
// the icon means colour is never the only signal.
export const PILLS = {
  // Features
  draft: { label: "Draft", icon: "doc", tone: "status-queued" },
  pending: { label: "Pending", icon: "clock", tone: "status-pending" },
  in_progress: { label: "In progress", icon: "play", tone: "status-running" },
  completed: { label: "Completed", icon: "check", tone: "status-completed" },
  abandoned: { label: "Abandoned", icon: "close", tone: "status-failed" },
  // Runs
  running: { label: "Running", icon: "play", tone: "status-running" },
  accepting: { label: "Accepting", icon: "clock", tone: "status-running" },
  succeeded: { label: "Succeeded", icon: "check", tone: "status-completed" },
  failed: { label: "Failed", icon: "close", tone: "status-failed" },
  blocked: { label: "Blocked", icon: "close", tone: "status-failed" },
  needs_review: { label: "Needs review", icon: "doc", tone: "status-waiting" },
  pause_requested: { label: "Pause requested", icon: "clock", tone: "status-waiting" },
  paused: { label: "Paused", icon: "pause", tone: "status-waiting" },
  resumed: { label: "Resumed", icon: "rotate", tone: "neutral" },
  design_files_needed: { label: "Design files needed", icon: "doc", tone: "status-waiting" },
  // Queue entries
  queued: { label: "Queued", icon: "dots", tone: "status-queued" },
  next: { label: "Next", icon: "play", tone: "status-queued" },
  waiting: { label: "Waiting", icon: "pause", tone: "status-waiting" },
  review: { label: "Waiting for review", icon: "doc", tone: "status-waiting" },
  skipped: { label: "No longer needed", icon: "close", tone: "neutral" },
  // Reviews
  open: { label: "Open", icon: "doc", tone: "status-waiting" },
  merged: { label: "Merged", icon: "check", tone: "status-completed" },
  closed: { label: "Closed", icon: "close", tone: "neutral" },
  resolved: { label: "Reviewed", icon: "check", tone: "status-completed" },
  unreadable: { label: "Unreadable", icon: "alert", tone: "neutral" },
  // Risk
  "risk-high": { label: "High risk", icon: "alert", tone: "risk-high" },
  "risk-medium": { label: "Medium risk", icon: "alert", tone: "risk-medium" },
  "risk-low": { label: "Low risk", icon: "check", tone: "risk-low" },
  "risk-none": { label: "No risk rating", icon: "dots", tone: "neutral" },
  "risk-resolved": { label: "Risks fixed", icon: "check", tone: "neutral" },
  // Freshness
  current: { label: "Current", icon: "check", tone: "freshness-fresh" },
  stale: { label: "Stale", icon: "alert", tone: "freshness-outdated" },
  unverified: { label: "Unverified", icon: "flag", tone: "neutral" },
};

// The tones a caller may name directly; anything else is drawn neutral.
const TONES = ["status-running", "status-completed", "status-failed", "status-waiting", "status-queued", "status-pending", "risk-low", "risk-medium", "risk-high", "freshness-fresh", "freshness-outdated", "neutral"];

// A word the table does not know is shown as itself, in sentence case, neutral, with the flag icon.
function sentence(word) {
  const text = String(word).replace(/_/g, " ").trim();
  return text === "" ? "Unknown" : text[0].toUpperCase() + text.slice(1);
}

// kind picks the words, icon and tone from PILLS; label, icon and tone override them. `title`, when
// given, is both the hover text and what a screen reader hears. Any other prop (a data- attribute, a
// className) is passed to the pill.
export function StatusPill({ kind, label, icon, tone, title, className, children, ...rest }) {
  const known = typeof kind === "string" && Object.hasOwn(PILLS, kind) ? PILLS[kind] : null;
  const shownTone = typeof tone === "string" && TONES.includes(tone) ? tone : known !== null ? known.tone : "neutral";
  const words = label ?? (known !== null ? known.label : sentence(kind ?? ""));
  const glyph = icon ?? (known !== null ? known.icon : "flag");
  return (
    <span className={"status-pill tone-" + shownTone + (className ? " " + className : "")} title={title} aria-label={title} {...rest}>
      <Icon name={glyph} size={16} />
      {children ?? <span>{words}</span>}
    </span>
  );
}

// The named badge kinds the dashboard tokens test keeps: each a status pill in its token's tone.
export const BADGES = {
  "status-running": { label: "Running", icon: "play" },
  "status-completed": { label: "Completed", icon: "check" },
  "status-failed": { label: "Failed", icon: "close" },
  "status-waiting": { label: "Waiting", icon: "pause" },
  "status-queued": { label: "Queued", icon: "dots" },
  "risk-low": { label: "Low risk", icon: "check" },
  "risk-medium": { label: "Medium risk", icon: "alert" },
  "risk-high": { label: "High risk", icon: "alert" },
  "risk-resolved": { label: "Risks fixed", icon: "check" },
  "freshness-fresh": { label: "Fresh", icon: "check" },
  "freshness-outdated": { label: "Outdated", icon: "alert" },
};

// A badge is a status pill named by its token; an unknown or omitted kind is a neutral "Unknown".
export function Badge({ kind, title }) {
  // Only an own entry counts, so a kind such as "constructor" cannot reach inherited properties.
  const known = typeof kind === "string" && Object.hasOwn(BADGES, kind);
  const { label, icon } = known ? BADGES[kind] : { label: "Unknown", icon: "flag" };
  return <StatusPill label={label} icon={icon} tone={known && TONES.includes(kind) ? kind : "neutral"} title={title} data-badge={known ? kind : undefined} />;
}
