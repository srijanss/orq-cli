# Reviews tab as cards with buttons

Today the Reviews tab is a plain text list. A review's link can only be opened with `u`/`y`, which
nothing on screen shows, and `draft` appears with no hint of what it means. The Orchestrations tab
already shows each run as a bordered card with buttons. The Reviews tab does the same.

## Behaviour

- Each review is a bordered card: line 1 holds the feature, project and a status chip; line 2 holds
  the risk, `stale` when it is, and the run id.
- Buttons sit on the card's first line, right-aligned: `[Open MR]`, `[Copy link]` and `[Details]`.
  The two link buttons appear only when the review has a link. `[Details]` is always there and does
  what Enter does.
- Clicking a button acts on that card, also after scrolling and under search or filters. The keys
  `Enter`, `u` and `y` still work.
- `draft` means the stored report is not current yet and no lifecycle is recorded. The footer says
  so while a draft review is selected.
- A window too short for cards falls back to the one-line rows as today.
