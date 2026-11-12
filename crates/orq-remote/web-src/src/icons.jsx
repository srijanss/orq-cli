// Line icons in the reference's style: 24px grid, 2px round strokes, drawn in the text colour.
// They are decoration only (the label or button name carries the meaning), so they are hidden
// from assistive technology.
const PATHS = {
  logo: "M12 3 3 8l9 5 9-5-9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5",
  home: "M3 11 12 3l9 8M5 9.5V21h5v-6h4v6h5V9.5",
  flag: "M5 21V4m0 1h12l-2 4 2 4H5",
  play: "M7 4v16l13-8L7 4Z",
  doc: "M6 3h9l4 4v14H6V3Zm8 0v5h5M9 13h7M9 17h7",
  gear: "M12.220 2h-.440a2 2 0 0 0-2 2v.180a2 2 0 0 1-1 1.730l-.430.250a2 2 0 0 1-2 0l-.150-.080a2 2 0 0 0-2.730.730l-.220.380a2 2 0 0 0 .730 2.730l.150.100a2 2 0 0 1 1 1.720v.510a2 2 0 0 1-1 1.740l-.150.090a2 2 0 0 0-.730 2.730l.220.380a2 2 0 0 0 2.730.730l.150-.080a2 2 0 0 1 2 0l.430.250a2 2 0 0 1 1 1.730V20a2 2 0 0 0 2 2h.440a2 2 0 0 0 2-2v-.180a2 2 0 0 1 1-1.730l.430-.250a2 2 0 0 1 2 0l.150.080a2 2 0 0 0 2.730-.730l.220-.390a2 2 0 0 0-.730-2.730l-.150-.080a2 2 0 0 1-1-1.740v-.500a2 2 0 0 1 1-1.740l.150-.090a2 2 0 0 0 .730-2.730l-.220-.380a2 2 0 0 0-2.730-.730l-.150.080a2 2 0 0 1-2 0l-.430-.250a2 2 0 0 1-1-1.730V4a2 2 0 0 0-2-2ZM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
  sun: "M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8Zm0-13v2m0 14v2M3 12h2m14 0h2M5.600 5.600 7 7m10 10 1.400 1.400M5.600 18.400 7 17M17 7l1.400-1.400",
  moon: "M20 14.500A8 8 0 0 1 9.500 4 8 8 0 1 0 20 14.500Z",
  monitor: "M3 5h18v11H3ZM8 20h8m-4-4v4",
  clock: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Zm0-14v5l3 2",
  layers: "m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5m-18 4 9 5 9-5",
  refresh: "M20 11a8 8 0 0 0-14.500-4M4 5v4h4m-4 4a8 8 0 0 0 14.500 4M20 19v-4h-4",
  chevronsLeft: "m11 17-5-5 5-5m7 10-5-5 5-5",
  chevronsRight: "m13 17 5-5-5-5M6 17l5-5-5-5",
  menu: "M4 6h16M4 12h16M4 18h16",
  logout: "M9 21H5V3h4m7 4 5 5-5 5m5-5H9",
  close: "M6 6l12 12M18 6 6 18",
  folder: "M3 6a2 2 0 0 1 2-2h4l2 3h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6Z",
  chevronRight: "m9 6 6 6-6 6",
  passkey: "M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4ZM16.5 7.500h.010",
  chevronDown: "m6 9 6 6 6-6",
  search: "m21 21-4.300-4.300M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Z",
  arrowLeft: "M19 12H5m6-6-6 6 6 6",
  check: "m5 12 5 5L20 7",
  chevronLeft: "m15 6-6 6 6 6",
  download: "M12 4v11m-5-5 5 5 5-5M5 20h14",
  sortDesc: "M3 6h11M3 12h7M3 18h3m12-12v12m-3-3 3 3 3-3",
  sortAsc: "M3 6h3m-3 6h7m-7 6h11m4 0V6m-3 3 3-3 3 3",
  alert: "M12 3 2 20h20L12 3Zm0 6v5m0 3h.010",
  cpu: "M7 7h10v10H7ZM9 3v4m6-4v4M9 17v4m6-4v4M3 9h4m-4 6h4m10-6h4m-4 6h4",
  sliders: "M4 6h9m4 0h3M4 12h3m4 0h9M4 18h11m4 0h1M15 4v4M9 10v4m8 2v4",
  external: "M14 4h6v6m0-6-9 9M18 14v6H4V6h6",
  pause: "M8 5v14m8-14v14",
  stop: "M6 6h12v12H6Z",
  trash: "M4 7h16M9 7V4h6v3m-9 0 1 13h10l1-13M10 11v6m4-6v6",
  rotate: "M4 12a8 8 0 1 0 2.300-5.700M4 4v4h4",
  arrowUp: "M12 19V5m-6 6 6-6 6 6",
  pencil: "M4 20h4L19 9l-4-4L4 16v4Zm9-13 4 4",
  plus: "M12 5v14M5 12h14",
  dots: "M5 12h.010M12 12h.010M19 12h.010",
};

export function Icon({ name, size = 20 }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      <path d={PATHS[name]} />
    </svg>
  );
}
