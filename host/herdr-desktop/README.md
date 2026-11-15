# shprd on the Herdr desktop

The Herdr app draws its whole interface from files: `layouts/<name>.json`,
`mods/<name>/` and `keys.json` in `~/.config/herdr-app` (or `$HERDR_APP_HOME`).
This folder is shprd's set of them.

```
layouts/shprd.json      the window: sidebar, coordinator area, tab bar, pane, board
mods/shprd-spaces/      the sidebar: the coordinator's space, then one row a task
                        space, with its word, its colour and what needs you
mods/shprd-status/      the status bar: the run, its counts, the last pass's failure
mods/shprd-tasks/       the board: one row a task, grouped by shprd's derived status
mods/shprd-pane/        one pane header per terminal: agent, task, state, buttons
mods/shprd-tabs/        the tab bar (each tab's agent, role and state) and the
                        side panel's buttons
mods/shprd-chrome/      the sidebar footer: settings, shprd's settings, the service
mods/shprd-accept/      the Accept card            (M10.5)
mods/shprd-task-pr/     the task's pull request    (M10.5)
mods/shprd-asks/        the needs-you cards        (M10.6)
mods/shprd-settings/    roles, agents, prompts     (M10.6)
mods/shprd-graph/       the graph: dagr in a terminal block, or the word on
                        where dagr is          (M10.7)
mods/shprd-coordinator/ the coordinator's chat     (M10.8)
mods/shprd-toasts/      toasts over the work area  (M10.4)
mods/lib/               the shared modules: tokens, panel parts, the board's
                        reading (`board.js`), the sidebar's (`spaces.js`), the
                        tab bar's (`tabstrip.js`), the toasts' (`toasts.js`), the
                        service client, the change watcher, and sync-shared's
                        source. Every mod carries a copy in its own `lib/`
                        because a mod may only import files inside its own
                        folder.
keys.json               the keys the layout expects, merged by the installer
bin/sync-shared         copies `mods/lib/*` into each mod's `lib/`; `--check`
bin/check               all of it: the copies, the unit tests, `herdr-app check`
test/                   the unit tests (node or bun, see below); `fixtures/` holds
                        a real service's answers and a real window's list
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

## The graph block

The side panel's last row is the run's graph: a stack whose note
(`mods/shprd-graph`) starts a terminal block running
`dagr view <repo>/.dagr/run.json`. dagr draws the run's tasks, attempts and
gates, and follows the file itself — the service rewrites it atomically after
every ledger write (`crates/shprd-server/src/service.rs`'s after-write step,
`shprd dagr sync`), so nothing relaunches it per write. The note is also what a
person sees when there is no dagr: the message names `graph.dagr`, the setting
that fixes it (`[graph] dagr = "/path/to/dagr"`, or
`shprd settings set graph.dagr /path/to/dagr`). `shprd doctor` says which probe
found it — the setting, `PATH` or the `herdr-dagr` plugin's checkout.

The terminal's node (`{ "terminal": "shprd-graph" }`) is **not** in
`layouts/shprd.json`, and the reason is a Herdr behaviour worth knowing: a
layout terminal block whose terminal does not exist when the layout loads is
pruned from the file, and the pruned file is written back. A shipped node would
therefore be gone before the mod could start the terminal it names — and the
file would stop matching what the installer wrote. So the note starts the
terminal itself (`hc run --bg`, in a shell loop that restarts `dagr view` in
place, because herdr-core keeps a terminal's address for its lifetime), puts the
node into the live tree with `layout.set`, and selects it. A new window repeats
that, which is why the note is what a window shows for a moment.

## Tests

The client, the change watcher and the footer's service start have unit tests
that need neither Herdr nor a service:

```
node --import ./test/boot.mjs --test test/
bun test --preload ./test/boot-bun.mjs test/
```

Both run the same files, `test/lib.test.mjs`, `test/chrome.test.mjs` and
`test/mods.test.mjs`; `test/stub-herdr.js` stands in for the app's `herdr` module
and `test/stub-gpui.js` for its element builders (`gpui-kit`, `gpui-base`).
`test/mods.test.mjs` drives the four view models with fixtures captured from a
real `shprd serve` (`test/fixtures/state.json`, `changes.json`) and a real
`herdr-app ls` of the window that showed it (`spaces.json`): the sidebar's rows,
the tab bar's pills, the toasts' news and the status bar's counts are all pure
functions of those, so what a screenshot shows is what a test asserts.

## Starting the service

The footer's third row starts it with **`hc run --bg`**, never the app's `exec`.
`exec` is `machine.exec`, which the Herdr docs and the M7ef spike both describe as
"no terminal, ≤600 s, everything it starts dies with it" — a server started
through it would be gone when the call ended. `hc run --bg` starts a herdr-core
terminal and returns at once, so the service outlives the click and keeps a
terminal called `shprd-serve` where a person can read its output. Measured in a
private session: the call returned in 0.3 s, and the service was still answering
`GET /api/health` minutes later.

`bin/check` runs all three. Every mod passes `herdr-app check` (the layout is
checked by a window drawing it, which is what the screenshot is of). The check
runs on a **copy** in a scratch folder — `herdr-app check` writes
`gpui-kit.d.ts` and `jsconfig.json` beside the mod it loads, and those are the
app's, not ours — and points `HERDR_APP_HOME` there too, so it never touches a
real window or this tree. `.gitignore` keeps them out if one is ever written.
