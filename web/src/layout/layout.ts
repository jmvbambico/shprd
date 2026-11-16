// The three layouts, and the width each one starts at.
//
// Shepherdr's web app grows from one column into three at exactly these
// numbers (M7d §2.6): a phone is narrower than 800 px, a tablet is 800–1023,
// and a desktop is 1024 px and up. The same numbers are CSS custom media in
// `styles.css`; keep the two in step, and let a test pin them.

/** Below this width the app is one column with bottom tabs. */
export const PHONE_MAX = 800;

/** At this width and up the app is the three-column desktop layout. */
export const DESKTOP_MIN = 1024;

/** A layout class, written to the shell as `data-layout`. */
export type Layout = "phone" | "tablet" | "desktop";

/** Which layout a viewport width gets. */
export function layoutFor(width: number): Layout {
  if (width < PHONE_MAX) {
    return "phone";
  }
  if (width < DESKTOP_MIN) {
    return "tablet";
  }
  return "desktop";
}

/** How many columns the shell draws: one, two or three. */
export function columnsFor(layout: Layout): 1 | 2 | 3 {
  switch (layout) {
    case "phone":
      return 1;
    case "tablet":
      return 2;
    case "desktop":
      return 3;
  }
}

/** The screens the shell switches between. */
export const SCREENS = ["tasks", "graph", "terminal", "chat", "settings"] as const;

/** One screen the shell can show. */
export type Screen = (typeof SCREENS)[number];

/** The words a screen's tab, heading and placeholder use. */
export const SCREEN_LABEL: Record<Screen, string> = {
  tasks: "Tasks",
  graph: "Graph",
  terminal: "Terminal",
  chat: "Chat",
  settings: "Settings",
};
