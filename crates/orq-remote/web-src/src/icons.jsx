// Line icons in the reference's style: 24px grid, 2px round strokes, drawn in the text colour.
// They are decoration only (the label or button name carries the meaning), so they are hidden
// from assistive technology.
const PATHS = {
  logo: "M12 3 3 8l9 5 9-5-9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5",
  home: "M3 11 12 3l9 8M5 9.5V21h5v-6h4v6h5V9.5",
  flag: "M5 21V4m0 1h12l-2 4 2 4H5",
  play: "M7 4v16l13-8L7 4Z",
  doc: "M6 3h9l4 4v14H6V3Zm8 0v5h5M9 13h7M9 17h7",
  gear: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM12 2v3m0 14v3M2 12h3m14 0h3M4.900 4.900 7 7m10 10 2.100 2.100M4.900 19.100 7 17M17 7l2.100-2.100",
  sun: "M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8Zm0-13v2m0 14v2M3 12h2m14 0h2M5.600 5.600 7 7m10 10 1.400 1.400M5.600 18.400 7 17M17 7l1.400-1.400",
  moon: "M20 14.500A8 8 0 0 1 9.500 4 8 8 0 1 0 20 14.500Z",
  refresh: "M20 11a8 8 0 0 0-14.500-4M4 5v4h4m-4 4a8 8 0 0 0 14.500 4M20 19v-4h-4",
  chevronsLeft: "m11 17-5-5 5-5m7 10-5-5 5-5",
  chevronsRight: "m13 17 5-5-5-5M6 17l5-5-5-5",
  menu: "M4 6h16M4 12h16M4 18h16",
  logout: "M9 21H5V3h4m7 4 5 5-5 5m5-5H9",
  close: "M6 6l12 12M18 6 6 18",
  folder: "M3 6a2 2 0 0 1 2-2h4l2 3h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6Z",
  chevronRight: "m9 6 6 6-6 6",
  chevronDown: "m6 9 6 6 6-6",
  cogs: "M9 6a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM9 2v2m0 8v2M3 8h2m8 0h-.5M4.800 3.800l1.400 1.400m5.600 5.600.4.4M13.200 3.800l-1.400 1.400M4.800 12.200l1.400-1.400M17 15a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm0-3v1.500m0 6.500v2m-5-5h1.500m6.500 0h2",
};

export function Icon({ name, size = 20 }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      <path d={PATHS[name]} />
    </svg>
  );
}
