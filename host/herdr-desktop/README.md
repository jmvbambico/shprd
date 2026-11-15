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
bin/check               all of it: the copies, the tests, `herdr-app check`
bin/gate-env            a private app home with the first-run screens answered
test/                   the unit tests (node or bun, see below); `fixtures/` holds
                        a real service's answers and a real window's list
```

Install it with `shprd install herdr-desktop` (and `--dry-run` to see the plan,
`--uninstall` to take back exactly what it wrote), then pick the layout in Herdr
with `ctrl-shift-l`.

## Installing from anywhere

`shprd install herdr-desktop` installs the desktop the **binary** carries, not the
one in whichever checkout the caller happens to stand in. The order is:

1. `--from <dir>`, when a person names a tree. A named tree must be one: a
   `--from` with no `layouts/` is refused rather than quietly replaced by the
   embedded copy.
2. `$SHPRD_HOST_DIR`.
3. `<cwd>/host/herdr-desktop`, then `<cwd>/../host/herdr-desktop` — a checkout or
   a worker's worktree, whose `crates/` sits one step down from the tree.
4. The checkout this binary was compiled from, in a **debug** build only. A
   release binary has no such path, which is the point.
5. The copy embedded in the binary at build time (`crates/shprd/build.rs`, the
   same shape as the shipped `data/`). This is what makes an installed `shprd`
   work with no checkout at all, and what the M10 gate's bug 6 was about: run
   from a worktree without `--from`, the installer used to look in the canonical
   `~/projects/shprd` and refuse.

`--dry-run --json` says which tree it used in its `source` field.

### A file Herdr pruned

Herdr resolves a layout terminal block by name when the layout loads and **writes
the layout file back without the block** when no such terminal exists (see "The
graph block" below). That is the app's edit, not the person's. The installer owns
the file its manifest still lists, so it recognises a prune — the two documents
are compared with their terminal blocks removed — and puts its own file back:

```
wrote …/layouts/shprd.json (put back after Herdr pruned it)
```

A change that is not a prune (a size, a colour, a block someone added) is the
person's and is still refused, with the file left exactly as they wrote it. So
the recovery for a pruned block is just `shprd install herdr-desktop`; nothing
has to be moved aside first.

## The mods talk to `shprd serve`

A mod has no Node, no `setTimeout`, no WebSocket, no `EventSource` and no stream
reader: its `fetch` is buffered. So the client (`mods/lib/client.js`) finds the
service the way Shepherdr's window does — read `service.json` from the state
directory, confirm its `id` at `GET /api/health`, then send the bearer token from
the `token` file beside it — and follows it with a long poll
(`GET /api/changes`) plus the change file the service rewrites after every
ledger write (`<state>/shprd/changes`, watched with `watchFolder`). See
`mods/lib/watch.js`.

### A service that restarted

A `shprd serve` that restarts is a different service: a new `id` in
`service.json` (possibly on the same port), a new board, a new run. A panel must
never keep painting the old one, so every mod follows two rules, both in
`mods/lib/client.js`:

* Before a read, `attach(client)` compares the id in `service.json` with the one
the panel's client was made from. A different id (or a record that is gone) is a
restart: a new client, and the panel drops what it cached and reads the whole
board again — which is how a task added while the service was down appears
without the layout being reloaded.
* A read of the board is **strict**: `GET /api/state` against a dead port does not
throw, it just runs out of time and answers nothing, so a `null` board is treated
as a failure. The panel drops its client, shows the failure, and the next look
discovers the service again. Treating that `null` as "nothing new" is exactly
what used to freeze the panels for ever (the M10 gate's bug 5).
## The graph block

The side panel's last row is the run's graph: a stack with a terminal block
running `dagr view <repo>/.dagr/run.json`, and the note (`mods/shprd-graph`) that
explains it when there is nothing to draw. dagr draws the run's tasks, attempts and
gates, and follows the file itself — the service rewrites it atomically after
every ledger write (`crates/shprd-server/src/service.rs`'s after-write step,
`shprd dagr sync`), so nothing relaunches it per write. The note is also what a
person sees when there is no dagr: the message names `graph.dagr`, the setting
that fixes it (`[graph] dagr = "/path/to/dagr"`, or
`shprd settings set graph.dagr /path/to/dagr`). `shprd doctor` says which probe
found it — the setting, `PATH` or the `herdr-dagr` plugin's checkout.

`layouts/shprd.json` holds the block: the stack `graph` with `graph-note` (the
mod) and `graph-term` (`{ "terminal": "shprd-graph" }`). The terminal is started
**before the window loads that layout**, by `shprd serve`
(`crates/shprd/src/cmd/graph.rs`): Herdr resolves a layout terminal block by name
when the layout loads and **prunes the block from the file** when no such terminal
exists (measured), so a block that had to start its own terminal would be gone
before it could. The service is up before the window is, so it does it there —
`hc run --bg`, one `shprd-graph` terminal per session, reusing a live one that
already draws this run and forgetting finished ones that would shadow the name.
The terminal runs a shell loop that waits for the document and restarts
`dagr view` when it exits (herdr-core keeps a terminal's address for its
lifetime, so the block's binding stays right). It is started only when the host
probe found a Herdr desktop; a machine on the herdr TUI has no window to draw a
block in, and asking a core anything starts one.

The mod is the note and the fallback. It reads `GET /api/graph` for where dagr is
and what to run, keeps its own `hc run` in case the service did not start one (a
service started before the window's core, a terminal someone ended), and selects
the terminal's child once it is live — a mod may not edit a layout's structure on
this build, so it only ever selects a node the layout already holds. If Herdr
pruned the block anyway (a window opened on a fresh install with no service
running), the note says so and gives the recovery: `shprd install herdr-desktop`,
which puts its own layout back (see "A file Herdr pruned" above).

## A private window for a test or a gate

Everything below is for a **private** Herdr, never the person's own: a private
`HC_SESSION`, a private `HERDR_APP_HOME`, and every `XDG_*` variable and `TMPDIR`
under one scratch folder — the recipe is M7ef §2.1, and the socket path has to fit
in 103 bytes, so the scratch folder is usually reached through a short symlink.

A fresh `HERDR_APP_HOME` opens with Herdr's first-run welcome over the layout, and
a `layout.switch` does not clear it. `bin/gate-env` writes the two keys the
welcome and the "What's new" screen read before the window starts, into a home
of your own:

```sh
# a private app home, with the first-run screens already answered
eval "$(HERDR_APP_HOME=/tmp/shs/app bin/gate-env --export)"

# the whole private environment, per M7ef §2.1
ln -sfn "$(mktemp -d)" /tmp/shs
export HC_SESSION=shprdgate \
       XDG_CONFIG_HOME=/tmp/shs/cfg XDG_STATE_HOME=/tmp/shs/state \
       XDG_DATA_HOME=/tmp/shs/data XDG_RUNTIME_DIR=/tmp/shs/run \
       XDG_CACHE_HOME=/tmp/shs/cache TMPDIR=/tmp/shs/tmp
unset HC_TERMINAL_ID HERDR_TAB_ID HERDR_PANE_ID HERDR_WORKSPACE_ID

shprd install herdr-desktop            # the binary's own copy
herdr-app                                # the private window
herdr-app do layout.switch '{"name":"shprd"}'
herdr-app shot /tmp/shs/shot.png
```

`bin/gate-env` refuses `$HOME/.config/herdr-app` outright: writing
`frame.welcomed` into a person's own window would tell it that it had already
shown them the welcome. `bin/check` runs that refusal as part of its own check.

## Tests

The client, the change watcher and the footer's service start have unit tests
that need neither Herdr nor a service:

```
node --import ./test/boot.mjs --test test/
bun test --preload ./test/boot-bun.mjs test/
```

Both run the same files, `test/lib.test.mjs`, `test/restart.test.mjs`,
`test/chrome.test.mjs`, `test/mods.test.mjs`, `test/settings.test.mjs`,
`test/asks.test.mjs`, `test/coordinator.test.mjs` and `test/graph.test.mjs`;
`test/stub-herdr.js` stands in for the app's `herdr` module and
`test/stub-gpui.js` for its element builders (`gpui-kit`, `gpui-base`).
`test/restart.test.mjs` is the one about a `shprd serve` that restarted: the
client's `attach` notices the new `id` in `service.json`, a board read that gets
no answer is a failure rather than "nothing new", and the tasks panel repaints
the new run's board without the layout being reloaded (the M10 gate's bug 5).
`test/mods.test.mjs` drives the four view models with fixtures captured from a
real `shprd serve` (`test/fixtures/state.json`, `changes.json`) and a real
`herdr-app ls` of the window that showed it (`spaces.json`): the sidebar's rows,
the tab bar's pills, the toasts' news and the status bar's counts are all pure
functions of those, so what a screenshot shows is what a test asserts.
`test/settings.test.mjs` and `test/asks.test.mjs` do the same for the settings
page and the needs-you cards, from the same service's `GET /api/settings`,
`GET /api/roster`, `GET /api/prompts`, `GET /api/state` and `GET /api/asks`
(`test/fixtures/settings.json`, `roster.json`, `prompts.json`, `needs-state.json`,
`needs-asks.json`): the rows, the layers, the picker choices, a refusal's words
and the cards are all read off those answers.

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
