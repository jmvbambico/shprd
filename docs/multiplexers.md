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
  `prefix+alt+u`. shprd detects the multiplexer from the pane's environment
  (`HERDR_PANE_ID` on herdr; the tuios variable is unverified).
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

## Unverified

- That the alt layer is free on both (`herdr --default-config`,
  `tuios keybinds free`).
- tuios's split-ratio parameter and its "inside tuios" variable.
- tuios accepts `type = "plugin_action"` with a herdr plugin action id. If tuios
  can host herdr plugins, herdr-mobile-relay may work there too.
