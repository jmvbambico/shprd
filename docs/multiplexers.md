# Multiplexers

Status: design.

## Layout

| | herdr | tuios |
|---|---|---|
| Per repo | one workspace `shprd · <repo>` | one session `shprd-<repo>` |
| Chat | tab 1 `shprd·chat` | workspace 1, window `shprd·chat` |
| Orchestrator | tab 2 `★ <cli>·orchestrator` | workspace 2 |
| Workers | one tab each | workspace 3, tiled windows |
| Reviewers | a tab per review round | workspace 4 |

Inside `shprd·chat`, the chat keeps 68% of the width. The right column holds
`dagr view` on top and the remote box below it.

Your own workspaces and sessions are never created, renamed or closed by shprd.

## Launch

- **Inside a multiplexer:** run `shprd up` in any pane of the repo, or press
  `prefix+alt+u`. shprd detects the multiplexer from the pane's environment:

  | Mux | "Inside" marker | Also set |
  |---|---|---|
  | herdr | `HERDR_ENV=1` | `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` |
  | tuios | `TUIOS_ENV=1` | `TUIOS_SOCKET`, `TUIOS_SESSION`, `TUIOS_WINDOW_ID`, `TUIOS_PANE_ID`, `TUIOS_PANE_TOKEN`, `TUIOS_PANE_GRANTS` |

  Check `TUIOS_ENV` first: tuios can also set `HERDR_ENV=1` in some panes (see
  below).
- **From a plain terminal:** `shprd up` starts or attaches the multiplexer set
  in config (`mux = "herdr" | "tuios"`, or `--mux`) and lands you in
  `shprd·chat`.

## Keys

The same chords on both. herdr uses its own prefix; tuios defaults to `ctrl+b`.

| Chord | Does |
|---|---|
| `prefix+alt+u` | `shprd up` |
| `prefix+alt+c` | go to `shprd·chat` |
| `prefix+alt+o` | go to the orchestrator |
| `prefix+alt+n` | jump to the next thing that needs you |
| `prefix+alt+r` | show or hide the remote box |
| `prefix+alt+s` | status popup |
| `prefix+alt+d` | doctor popup |

Why the alt layer: plain prefix letters collide differently on each mux. tuios
already uses `i o f e t u b g j k v …` and herdr uses `s q o w g c p n x z r b e v …`.
Only `a d m y` are free on both.

`shprd setup` writes the bindings between `# >>> shprd` and `# <<< shprd`
markers:

- herdr: `[[keys.command]]` in `~/.config/herdr/config.toml`;
- tuios: `[[keybindings.command]]` in its config.

Both accept `shell`, `pane` and `popup` command types.

Verified 2026-10-27:

- **herdr:** no default binding uses `alt`. `prefix+alt+<letter>` is documented
  syntax for `[[keys.command]]`.
- **tuios:** from source, prefix `ctrl+b` (`internal/config/settings.go:953`),
  and no default prefix binding uses `alt`. Its own docs use `prefix+alt+n/s/g`
  as examples. Command types are `scratch | popup | pane | shell`.
- `alt` delivery depends on the terminal (and tmux, if nested). `shprd doctor`
  should test it.

## tuios speaks herdr

tuios v0.10.1 contains a deliberate herdr compatibility layer (tracked against
herdr 0.9.3):

- **Plugins.** It parses `herdr-plugin.toml` and runs startup, actions, events,
  panes and link handlers (`internal/herdrplugin/`). Its daemon answers
  `plugin.*` requests (`internal/session/herdr_plugins.go:24-38`).
- **Socket.** It serves a herdr-compatible socket with about 60 herdr methods
  (`session.snapshot`, `pane.split/read/send_text/send_keys`, `agent.*`,
  `worktree.*`, `events.wait`) and answers `unsupported` for the rest
  (`internal/session/herdr_api.go:254-319`).
- **CLI shim.** `HERDR_BIN_PATH` points at a `herdr` → tuios link
  (`internal/herdrcli/`).
- **One condition.** `HERDR_ENV`, `HERDR_SOCKET_PATH` and `HERDR_PANE_ID` are
  injected only into panes running a known agent harness, or into every pane
  with `[agents] herdr_protocol = "always"` (`internal/config/agents.go:71-75`).

Consequences:

- **herdr-mobile-relay may run on tuios** with `herdr_protocol = "always"`.
  Whether every method the relay needs is supported still needs checking.
- **The herdr adapter may work on tuios through this layer.** M8 starts by
  running shprd's herdr adapter against it, then adds the tuios-native extras
  (pane grants, `ask-human`, resumable events).

## Unverified

- tuios's split-ratio parameter.
- `prefix+alt` at runtime in a real terminal on tuios (`tuios keybinds doctor`).
- Which herdr methods herdr-mobile-relay calls that tuios reports as `unsupported`.
