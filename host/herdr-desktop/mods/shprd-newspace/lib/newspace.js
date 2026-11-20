// The New space dialog's words and rules, as plain data: what the field's text
// names, whether to offer "Make it a git repo", and the git commands Create runs.
// No Herdr, no I/O, so a test runs it as it is.
//
// Ported from Shepherdr's New space dialog (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/newspace.js` (the words, `NEW_REPO`, `tilde`, `untilde`, `base`,
//   `pathOf`) and `mods/newspace/main.js` (the dialog's order: folder field,
//   Browse…, the git line, Cancel and Create)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/newspace.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

/** The dialog's words. */
export const WORDS = {
  title: "New space",
  label: "Folder",
  placeholder: "~/path/to/folder",
  browse: "Browse…",
  missing: "This folder doesn't exist. It will be created.",
  makeRepo: "Make it a git repo",
  makeRepoWhy: "so each task gets its own worktree",
  isRepo: "A git repo: each task gets its own worktree",
  create: "Create space",
  creating: "Creating…",
  cancel: "Cancel",
  cantMake: (dir) => `Couldn't make ${dir}.`,
  cantRepo: (why) => `Couldn't make it a git repo: ${why || "git failed"}`,
  cantSpace: (why) => `Couldn't make the space: ${why || "Herdr said no"}`,
};

/** A new repo, as Shepherdr's: `main`, one empty commit, no hooks or signing in the way. */
export const NEW_REPO = [
  ["init", "-q", "--initial-branch=main"],
  [
    "-c", "user.useConfigOnly=false",
    "-c", "core.hooksPath=/dev/null",
    "-c", "commit.gpgsign=false",
    "commit", "-q", "--allow-empty", "-m", "First commit",
  ],
];

/** A path as the dialog shows it: from home as `~`. */
export const tilde = (path, home) =>
  path === home ? "~" : home && path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;

/** A path with a leading `~` made whole. */
export const untilde = (path, home) =>
  path === "~" ? home : path.startsWith("~/") ? `${home}${path.slice(1)}` : path;

/** The last part of a path: the space's name. */
export const base = (path) => String(path ?? "").split("/").filter(Boolean).pop() ?? String(path ?? "");

/** The folder a field's text names, or null when it is not a path. */
export function pathOf(text, home) {
  const field = String(text ?? "").trim();
  if (!/^[~/]/.test(field)) return null;
  return untilde(field.length > 1 ? field.replace(/\/+$/, "") : field, home);
}

/**
 * Whether the dialog offers "Make it a git repo": the folder is known not to be in
 * one (`inRepo === false`), and is not home itself.
 */
export function offersRepo(folder, inRepo, home) {
  return Boolean(folder) && folder !== home && inRepo === false;
}

/** The space's name: the folder's own name. */
export function spaceName(folder) {
  return base(folder) || "space";
}

/** The `herdr-app do space.new` arguments for a folder. */
export function spaceNewArgs(folder) {
  return ["do", "space.new", "--folder", folder, "--name", spaceName(folder)];
}

/** The tab the new space opens on: the shprd assistant's chat view, named like Shepherdr's. */
export const ASSISTANT_TAB = "Assistant";

/**
 * The `herdr-app do tab.new` arguments for the assistant's tab in a space: the
 * chat mod as the tab's only pane, on the coordinator's conversation. Until
 * `shprd serve` is up the chat says it is starting; it never shows a shell.
 */
export function assistantTabArgs(spaceId) {
  return [
    "do", "tab.new", "--space", spaceId, "--name", ASSISTANT_TAB, "--mod", "shprd-chat",
    "--props", JSON.stringify({ space: "coordinator" }),
  ];
}

/**
 * The space `space.new` just made and the tabs the window put in it, read from
 * two `herdr-app ls` answers (before and after): the one space that was not there
 * before. Only those tabs are the window's own; a tab the person opens later is
 * never in this list. Null when it cannot be told (none or several are new).
 */
export function newSpaceOf(before, after) {
  const known = new Set((before?.spaces ?? []).map((space) => space.id));
  const fresh = (after?.spaces ?? []).filter((space) => space?.id && !known.has(space.id));
  if (fresh.length !== 1) return null;
  return { id: fresh[0].id, tabs: (fresh[0].tabs ?? []).map((tab) => tab.id).filter(Boolean) };
}
