# Architecture

Status: design, with the M11 service, web app and desktop built. Language: Rust
(service) and TypeScript (web app and the desktop's mods).

## Three layers

```
┌───────────────────────────────────────────────────────────────────────┐
│ CORE (host-agnostic, most of the code)                                │
│  engine: event log, fold, transition rules, scheduler, gate runner    │
│  contract: AGENTS.md + .shprd/config.toml                             │
│  roster: CLI discovery, harness table, models, fallback, quota        │
│  git: plain-git worktrees, integration branch, cherry-pick, clashes   │
│  review routing, landing via gh, janitor                              │
│  views: dagr run.json, brief, status, PR body                         │
├───────────────────────────────────────────────────────────────────────┤
│ HOST ADAPTER (one per multiplexer, thin, capability-negotiated)       │
│  spawn agent in pane · send prompt · read screen · agent-state events │
│  layout · tab/window naming · notifications · keybind install         │
│  optional: pane grants, human ask/answer, resumable event stream      │
├───────────────────────────────────────────────────────────────────────┤
│ REMOTE                                                                │
│  web app + login + tunnels (shprd-remote), the same on every host     │
│  herdr TUI → herdr-mobile-relay   tuios → tuios-web / SSH / push      │
└───────────────────────────────────────────────────────────────────────┘
```

## The engine owns the truth

The LLMs produce intent. Code produces state.

```
 orchestrator / workers / you          engine                       views
 ───────────────────────────           ──────                       ─────
 shprd agent task add …   ─┐
 shprd agent spawn …       │  validate  append to              ┌─► .dagr/run.json (dagr view)
 shprd agent report …      ├──────────► .shprd/events.jsonl ─fold─►├─► shprd brief
 shprd agent gate …        │            (one writer)            ├─► PR body
 shprd agent escalate …   ─┘                                    └─► status, phone cards
 host events (agent state, pane exit) ─────────────► (same log)
```

- **Event log.** Append-only JSONL, one event per line:
  `{seq, at, actor, kind, task, payload}`. Current state is a fold over the
  log. After a crash, the engine replays it.
- **Verbs are the only write path.** Agents call `shprd agent …`. The engine
  checks every transition and refuses illegal ones. For example, it won't
  integrate past an open review failure unless given an explicit, logged
  override.
- **Projectors are pure functions** from events to a view. The dagr run file is
  written as tmp, then `dagr check --strict`, then rename. No agent ever writes
  `run.json`, `brief` or the PR body.
- **Evidence tiers fall out of the log.** `verified` means the engine ran the
  check and recorded its exit code; `reported` means an agent said so.

## Chat, terminals and the coordinator

The coordinator is a **real CLI** shprd starts in the repo's one space. On the
Herdr desktop and in the web app it is shown as the space's **Assistant chat**,
not as a terminal tab. A task's worker and reviewer are real CLIs too, run as
labelled `hc` children with no tab or space of their own; `GET /api/state` lists
them as `live_agents`. The service:

- reads the program's own conversation file and serves it as a transcript
  (`GET /api/chat`), which the chat draws with the event cards from the log;
- types what you send into the agent's pane through the host (`POST /api/chat`);
- pushes an agent's real terminal to a page over a socket (`GET /api/terminal`):
  watch by default, type on demand. A task's thread is Chat | Terminal.

The desktop, the web app and the phone therefore all see the same session. See
[Agents](agents.md) for where each CLI keeps its conversation, and
[decisions](decisions.md#2026-11-18) for why there is no tab per agent.

## Crates, the web app and the desktop

| Part | What it is |
|---|---|
| `shprd-core`, `shprd-views`, `shprd-git`, `shprd-agents`, `shprd-watch` | the engine: events and fold, projectors, git, the agent CLIs as data, the watchdog |
| `shprd-mux`, `shprd-mux-hc`, `shprd-mux-herdr` | the host adapter trait, and its two adapters: Herdr desktop through herdr-core (`hc`), and the herdr TUI |
| `shprd-server` | the service: the HTTP and WebSocket API, the change hub, the tick, the chat, terminal and tunnel routes, and the embedded web app |
| `shprd-remote` | the password store, the sessions, the gate (`Host`, credential, `Origin`, `x-shprd`, freshness), the rate limits, and the tunnels (Tailscale serve, cloudflared, ngrok, tunnl.gg) with the QR. The gate's decision is a plain function over a request, so its table is a unit test. See [Remote access](remote.md) |
| `shprd` | the binary: the CLI, and the wiring that gives the service a host, a harness table and its state directory |
| `web/` | the web app: Vite, React 19 and TypeScript, with xterm.js for terminals. Its types in `web/src/gen` are generated from the service's DTOs by ts-rs (`cargo run -p shprd-server --example export-ts`), so a DTO change that is not regenerated fails CI. `npm run build` makes `web/dist`, which `shprd-server` embeds with rust-embed: one binary serves the page and the API from one origin. A release build refuses to build without `web/dist` |
| `host/herdr-desktop/` | the Herdr desktop's layout, mods and keys; `shprd install herdr-desktop` puts them in the app's home. The binary carries a copy |

### The desktop's mods

The Herdr app draws itself from a layout and JS mods; these are thin over
`shprd serve` (they read it with the bearer token, a long poll and the change
file). Shared code is in `mods/lib/` and copied into each mod by
`bin/sync-shared`. See `host/herdr-desktop/README.md` for each.

| Mod | Draws |
|---|---|
| `shprd-spaces`, `shprd-agents` | the sidebar: the spaces tree (space, live tasks, working agent) and the Agents pane |
| `shprd-tasks` | the Tasks | Graph panel; the graph is drawn natively from the board |
| `shprd-task-view`, `shprd-accept`, `shprd-task-pr` | a task's thread head, its read-only terminal fallback and artifacts, the Accept card, the pull request |
| `shprd-chat` | chat: the task thread's default view, the coordinator's Assistant chat, the question, permission and plan cards |
| `shprd-pane` | a terminal pane's header, with Chat | Terminal |
| `shprd-tabs`, `shprd-new-menu`, `shprd-menu`, `shprd-prompt` | the tab bar, the "+" menu, the context menus, the rename prompt |
| `shprd-status`, `shprd-toasts` | the status bar, toasts |
| `shprd-settings`, `shprd-newspace`, `shprd-jump`, `shprd-keybinds`, `shprd-chrome` | the settings dialog, New space, Jump, Keybinds, the sidebar's foot |

## Host adapter capabilities

The core asks the adapter what it can do and degrades when a capability is
missing. It doesn't design to the lowest common denominator.

| Capability | herdr | tuios |
|---|---|---|
| spawn, prompt, read screen, agent state | yes | yes |
| split with ratio | yes | unverified |
| resumable event stream | no | yes (`subscribe` with `after_seq`) |
| ask the human natively | no | yes (`ask-human`) |
| pane permission grants | no | yes |

Where a host has grants, workers run without `admin`. Where it doesn't, the same
rule is advisory and every bypass is logged. See [Self-governance](governance.md).

## What lives where on disk

| Path | Owner | Contents |
|---|---|---|
| `~/.config/shprd/config.toml` | you, via `shprd setup` | roster, skills, workspace mode, janitor settings |
| `<repo>/.shprd/config.toml` | you | per-repo overrides, merged field by field |
| `<repo>/.shprd/events.jsonl` | engine | the event log |
| `<repo>/.dagr/run.json` | engine (projector) | the task graph for `dagr view` |
| `~/.shprd/wt/<repo>/<run>/<task>` | engine | worker worktrees |
| `refs/shprd/archive/…` | engine | archived work, kept before any delete |
