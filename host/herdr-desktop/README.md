# shprd on the Herdr desktop

The Herdr app draws its whole interface from files: `layouts/<name>.json`,
`mods/<name>/` and `keys.json` in `~/.config/herdr-app` (or `$HERDR_APP_HOME`).
This folder is shprd's set of them.

```
layouts/shprd.json      the window: sidebar, coordinator area, tab bar, pane, board
mods/shprd-status/      the status bar: the run, its counts, the last pass's failure
mods/shprd-tasks/       the board: one row a task, grouped by shprd's derived status
mods/shprd-pane/        one pane header per terminal: agent, task, state, buttons
mods/shprd-tabs/        the tab bar and the side panel's buttons
mods/shprd-chrome/      the sidebar footer: settings, shprd's settings, the service
mods/shprd-accept/      the Accept card            (M10.5)
mods/shprd-task-pr/     the task's pull request    (M10.5)
mods/shprd-asks/        the needs-you cards        (M10.6)
mods/shprd-settings/    roles, agents, prompts     (M10.6)
mods/shprd-graph/       dagr in a terminal block   (M10.7)
mods/shprd-coordinator/ the coordinator's chat     (M10.8)
mods/shprd-toasts/      toasts over the work area  (M10.4)
mods/lib/               the shared modules: tokens, panel parts, the board's
                        reading, the service client, the change watcher, and
                        sync-shared's source. Every mod carries a copy in its
                        own `lib/` because a mod may only import files inside
                        its own folder.
keys.json               the keys the layout expects, merged by the installer
bin/sync-shared         copies `mods/lib/*` into each mod's `lib/`; `--check`
bin/check               all of it: the copies, the unit tests, `herdr-app check`
test/                   the client's unit tests (node or bun, see below)
```

Install it with `shprd install herdr-desktop` (and `--dry-run` to see the plan,
`--uninstall` to take back exactly what it wrote), then pick the layout in Herdr
with `ctrl-shift-l`.

## The mods talk to `shprd serve`

A mod has no Node, no `setTimeout`, no WebSocket, no `EventSource` and no stream
reader: its `fetch` is buffered. So the client (`mods/lib/client.js`) finds the
service the way Shepherdr's window does — read `service.json` from the state
directory, confirm its `id` at `GET /api/health`, then send the bearer token from
the `token` file beside it — and follows it with a long poll
(`GET /api/changes`) plus the change file the service rewrites after every
ledger write (`<state>/shprd/changes`, watched with `watchFolder`). See
`mods/lib/watch.js`.

## Tests

The client and the change watcher have unit tests that need neither Herdr nor a
service:

```
node --import ./test/boot.mjs --test test/
bun test --preload ./test/boot-bun.mjs test/
```

Both run the same file, `test/lib.test.mjs`; `test/stub-herdr.js` stands in for
the app's `herdr` module.

`bin/check` runs all three. Every mod passes `herdr-app check` (the layout is
checked by a window drawing it, which is what the screenshot is of); the check
points `HERDR_APP_HOME` at a scratch folder, so it never touches a real window.
