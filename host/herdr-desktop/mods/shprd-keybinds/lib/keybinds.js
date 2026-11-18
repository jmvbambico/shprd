// The Keybinds dialog's words: each key as people read it, each action in plain
// words, grouped (Tabs, Spaces, Tasks, Panes, Agents, Terminal, App).
//
// The window's keys are read live (`keys()`); this module only names them, so
// the whole dialog is testable with a list of bindings and no window.
//
// Ported from Shepherdr's Keybinds core (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Path: `mods/keybinds/core/keys.js` (`NAMES`, `groupOf`, `nameOf`, `keyText`,
//   `grouped`, `keyFound`, `KEYBINDS`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the names of shprd's own commands (`shprd-jump`,
// `shprd-keybinds`, `shprd-tabs.new-task`, the side panel's toggles), a *Tasks*
// group for them, and the layout section's name (`shprd`, not `shepherdr`).
//
// Shared: the source is `mods/lib/keybinds.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

const DIRS = { left: "left", right: "right", up: "up", down: "down" };

/**
 * Names for actions whose words depend on their argument, and for mods' own
 * commands (which `commands()` does not list). `(arg) => name`.
 */
export const NAMES = {
  "tab.new": () => "New tab",
  "tab.close": () => "Close tab",
  "tab.rename": () => "Rename tab",
  "tab.next": () => "Next tab",
  "tab.prev": () => "Previous tab",
  "space.new": () => "New space",
  "space.rename": () => "Rename space",
  "space.next": () => "Next space",
  "space.prev": () => "Previous space",
  "pane.split": (d) => (d === "down" ? "Split down" : "Split right"),
  "pane.go": (d) => `Focus the pane ${DIRS[d] ?? d}`,
  "pane.swap": (d) => `Swap with the pane ${DIRS[d] ?? d}`,
  "pane.close": () => "Close pane",
  "pane.zoom": () => "Zoom pane, or show all",
  "layout.toggle": (id) =>
    ({ sidebar: "Show or hide the sidebar", panel: "Show or hide the side panel" })[id] ??
    `Show or hide ${id}`,
  "layout.show": (id) => ({ asks: "Show the Asks panel" })[id] ?? `Show ${id}`,
  "layout.switch": () => "Next layout",
  "mod.open": (name) =>
    ({
      "shprd-jump": "Jump to a task, space or the coordinator",
      "shprd-keybinds": "Keybinds",
    })[name] ?? `Open ${name}`,
  "app.help": () => "Keybinds",
  "app.palette": () => "Command palette",
  "app.settings": () => "Settings",
  "app.zoom_in": () => "Zoom in",
  "app.zoom_out": () => "Zoom out",
  "app.zoom_reset": () => "Actual size",
  "terminal.copy": () => "Copy",
  "terminal.paste": () => "Paste",
  "shprd-tabs.new-task": () => "New task",
  "shprd-tasks.add": () => "New task",
};

/** What a terminal key types, by its text. */
const SENDS = { "\u0017": "Delete the word before", "\u001bb": "Word left", "\u001bf": "Word right" };

/** The group order; a mod's own group goes between Terminal and App. */
export const ORDER = ["Tabs", "Spaces", "Tasks", "Panes", "Agents", "Terminal", "App"];

/** The group an action goes in. */
export function groupOf(section, action) {
  if (section === "terminal") return "Terminal";
  if (section.startsWith("mod:")) {
    return section.slice(4).replace(/^./, (c) => c.toUpperCase()).replace(/-/g, " ");
  }
  if (/^tab\./.test(action)) return "Tabs";
  if (/^space\./.test(action)) return "Spaces";
  if (/^shprd-(tabs|tasks)\.(new-task|add)$/.test(action) || /^layout\.(toggle|show):(panel|asks)$/.test(action)) {
    return "Tasks";
  }
  if (/^pane\./.test(action)) return "Panes";
  if (/^agent\./.test(action)) return "Agents";
  return "App";
}

/**
 * An action in words. `about` maps a command name to its description (Herdr's
 * `commands()`), the fallback for a command this module has no name for.
 */
export function nameOf(action, about = new Map()) {
  if (action.startsWith("send:")) return SENDS[action.slice(5)] ?? "Type text";
  const [name, arg] = action.split(/:(.*)/s);
  if (NAMES[name]) return NAMES[name](arg);
  // The description's first clause: "Show the next agent, in the order…" → "Show the next agent".
  const said = (about.get(name) ?? "").split(/\s*[(:;,]|\.(\s|$)/)[0].trim();
  if (said) return said;
  const last = name.split(".").pop().replace(/[-_]/g, " ");
  return last[0].toUpperCase() + last.slice(1);
}

const SPECIAL = { pageup: "PageUp", pagedown: "PageDown", escape: "Esc", backspace: "Backspace" };

/** keys.json's `ctrl-shift-t` as people read it: `Ctrl+Shift+T` (`⌃⇧T` on macOS). */
export function keyText(stroke, mac = false) {
  const names = mac
    ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" }
    : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: mac ? "cmd" : "ctrl", super: "cmd", win: "cmd" };
  return stroke
    .trim()
    .split(/\s+/)
    .map((one) => {
      const parts = one.split("-");
      let key = parts.pop();
      if (key === "") {
        key = "-";
        parts.pop();
      }
      const held = new Set(parts.map((m) => alias[m.toLowerCase()] ?? m.toLowerCase()));
      if (/^[A-Z]$/.test(key)) held.add("shift");
      const label =
        SPECIAL[key.toLowerCase()] ??
        (key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + key.slice(1).toLowerCase());
      return [...Object.keys(names).filter((m) => held.has(m)).map((m) => names[m]), label].join(
        mac ? "" : "+",
      );
    })
    .join(" ");
}

/**
 * The rows, grouped: `[{group, rows: [{name, keys: [text]}]}]`.
 *
 * `list` is Herdr's `keys()`; `layout` is the shown layout's name, whose own
 * section's key replaces the global one (other layouts' sections are left out).
 * One row per name with all its keys; "Show tab 1" … "Show tab 9" fold into one
 * row, `Alt+1–9`.
 */
export function grouped(list, { layout = "", about = new Map(), mac = false } = {}) {
  const shown = `layout:${layout ?? ""}`;
  const all = (list ?? []).filter(
    (k) =>
      k.action &&
      k.action !== "terminal" &&
      !(k.section.startsWith("layout:") && k.section !== shown),
  );
  const mine = new Set(all.filter((k) => k.section === shown).map((k) => k.keys));
  const live = all.filter((k) => !(k.section === "global" && mine.has(k.keys)));
  const groups = new Map();
  for (const k of live) {
    const group = groupOf(k.section, k.action);
    const series = /^(tab|space)\.select:([1-9])$/.exec(k.action);
    const lead = /^(.*-)?([1-9])$/.exec(k.keys.trim());
    const name = series && lead ? `Show ${series[1]} 1–9` : nameOf(k.action, about);
    if (!groups.has(group)) groups.set(group, new Map());
    const rows = groups.get(group);
    if (!rows.has(name)) rows.set(name, []);
    const text = series && lead ? `${keyText(`${lead[1] ?? ""}1`, mac)}–9` : keyText(k.keys, mac);
    if (!rows.get(name).includes(text)) rows.get(name).push(text);
  }
  const rank = (g) => (ORDER.includes(g) ? ORDER.indexOf(g) : ORDER.length - 1.5);
  return [...groups]
    .sort((a, b) => rank(a[0]) - rank(b[0]))
    .map(([group, rows]) => ({ group, rows: [...rows].map(([name, keys]) => ({ name, keys })) }));
}

/** The dialog's filter: every word typed is in the group, the name or the keys. */
export function keyFound(filter, group, row) {
  const words = String(filter ?? "").toLowerCase().split(/\s+/).filter(Boolean);
  return words.every((w) => `${group} ${row.name} ${row.keys.join(" ")}`.toLowerCase().includes(w));
}

/** The dialog's words. */
export const KEYBINDS = { title: "Keybinds", filter: "Filter keys", none: "No keys match." };
