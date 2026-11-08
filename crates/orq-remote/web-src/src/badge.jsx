// A badge always shows its icon and its words, so colour is never the only signal.
export const BADGES = {
  "status-running": { label: "Running", icon: "▶" },
  "status-completed": { label: "Completed", icon: "✔" },
  "status-failed": { label: "Failed", icon: "✖" },
  "status-waiting": { label: "Waiting", icon: "⏸" },
  "status-queued": { label: "Queued", icon: "⋯" },
  "risk-low": { label: "Low risk", icon: "▽" },
  "risk-medium": { label: "Medium risk", icon: "◇" },
  "risk-high": { label: "High risk", icon: "▲" },
  "freshness-fresh": { label: "Fresh", icon: "●" },
  "freshness-outdated": { label: "Outdated", icon: "○" },
};

export function Badge({ kind }) {
  const { label, icon } = BADGES[kind];
  return (
    <span className={`badge badge-${kind}`}>
      <span aria-hidden="true">{icon}</span>
      <span>{label}</span>
    </span>
  );
}
