# Build plan

Status: M0–M6 done (gate passed 2026-10-29). Re-planned from M7 on 2026-11-14:
see "M7 onward · the pivot". M7 and M8 done 2026-11-14; M9 gate passed 2026-11-15;
M10 gate passed 2026-11-16 (gates below). M11 finished on `integration/m11`
(2026-11-22); its gate result and known gaps are under M11. Language: Rust (service) and TypeScript (web app).

The plan is cut into milestones. Each one ends at a **gate**: a demo you can run
and a check that has to pass before the next milestone starts. Inside a
milestone, tasks are split by directory so several agents can work in parallel
without touching the same files (see "Parallel lanes").

```
M0 foundations ─► M1 engine ─┬─► M2 git ─────────┐
                             ├─► M3 roster ──────┼─► M5 watchdog ─┐
                             └─► M4 herdr + up ──┘                ├─► M6 review & land ─► ★ dogfood ─► M7… (re-planned, see below)
                                                                  │
                                       (M3 also feeds M6) ────────┘
```

★ **Dogfood point:** after M6, shprd can run its own development from the
command line, without the TUI. From then on we build shprd with shprd and leave
og.

---

## Workspace layout (decided in M0; changed from M7, see "Workspace changes")

```
crates/
  shprd-core/       events, fold, task model, transition rules, config, tick planner   (no I/O beyond the log file)
  shprd-views/      projectors: dagr run.json, brief, status, PR body, coordinator news (pure functions of events)
  shprd-git/        worktrees, per-task landing, forge (PRs), archive refs, janitor
  shprd-agents/     harness table + adapters per agent CLI, roles and settings, detection, models, quota, asks
  shprd-watch/      watchdog signals, provider-error classes, the fallback ladder
  shprd-mux/        host adapter trait + capability flags
  shprd-mux-herdr/  herdr adapter
  shprd-mux-hc/     Herdr desktop through herdr-core (`hc`)
  shprd-remote/     the web login, sessions, gate and tunnels (M11)
  shprd-server/     the service: HTTP + WebSocket API, auth, event hub, tick loop
  shprd/            the binary: clap CLI, wiring
data/
  harness/*.toml    one file per agent CLI
  models/           static model catalogs
  templates/        shipped role prompts
  skills/compat.toml
web/src/gen/        TypeScript bindings generated from the service's DTOs (ts-rs)
```

Crate boundaries are the lane boundaries. Two agents never work in the same
crate in the same milestone unless the task says so.

## Gates (proposed for AGENTS.md)

| Gate | Command | Who runs it |
|---|---|---|
| format | `cargo fmt --all --check` | workers, CI |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | workers, CI |
| unit tests | `cargo test --workspace` | workers, CI |
| integration tests | `cargo test --workspace --features it -- --test-threads=1` | integration branch only |

Integration tests create real git repos in temp dirs and use a **fake mux**, so
they need no herdr, tuios or model calls. Tests that need a real CLI or mux are
marked `#[ignore]` and run by hand at milestone gates.

---

## M0 · Foundations

Goal: a repo that agents can work in safely.

| ID | Task | Scope | Size |
|---|---|---|---|
| M0.1 | `AGENTS.md`: gates, branch policy, review rule, never-write paths, commit style | `AGENTS.md` | S (docs) |
| M0.2 | Cargo workspace with the crates above as empty libs, the `shprd` binary printing `--version`, shared lints in `Cargo.toml` | `Cargo.toml`, `crates/*/` | S |
| M0.3 | CI: GitHub Actions running format, lint and unit tests on push and PR, macOS + Linux | `.github/workflows/` | S |
| M0.4 | `data/harness/` schema (a `HarnessSpec` serde type in `shprd-roster`) plus the five CLI files, filled from what we verified in [Agents](agents.md) | `data/harness/`, `crates/shprd-roster/src/spec.rs` | M |

Order: M0.1 → M0.2 → (M0.3 ∥ M0.4).

**Gate:** CI green on `main`. `cargo run -- --version` works. `AGENTS.md`
approved by you.

---

## M1 · Engine core (no git, no mux)

Goal: the event log is the truth, and every view is rendered from it.

| ID | Task | Scope | Size |
|---|---|---|---|
| M1.1 | Event types (`TaskAdded`, `Spawned`, `Reported`, `GatePassed`, `GateFailed`, `Integrated`, `Dry`, `HandedOff`, `Escalated`, `Override`, …), append-only JSONL log with fsync and a file lock, replay | `shprd-core/src/{event,log}.rs` | M |
| M1.2 | Fold to `RunState`; transition rules returning typed errors (e.g. integrating with an open failed gate is refused unless there's an `Override`) | `shprd-core/src/{state,rules}.rs` | M |
| M1.3 | Config: global + repo TOML, merged field by field; roster, skills, workspace, janitor sections | `shprd-core/src/config.rs` | S |
| M1.4 | dagr projector: events → dagr v3 document; tmp → `dagr check --strict` (if dagr is on PATH) → rename; golden-file tests | `shprd-views/src/dagr.rs` | M |
| M1.5 | `brief`, `status` and PR-body projectors, golden-file tests | `shprd-views/src/{brief,status,pr}.rs` | S |
| M1.6 | CLI: `shprd agent task add / report / gate / escalate`, `shprd brief`, `shprd status`, all with `--json` | `crates/shprd/src/cmd/` | M |

Lanes: (M1.1 → M1.2) ∥ M1.3; then M1.4 ∥ M1.5 ∥ M1.6.

**Gate:** a scripted fixture run (add 3 tasks, report, fail a gate, override,
pass) produces a `run.json` that passes `dagr check --strict`, plus stable
`brief` and PR-body output. Replaying the log after deleting all derived files
reproduces them byte for byte.

---

## M2 · Git layer

Goal: worktrees, scopes, integration and cleanup, all owned by the engine.

| ID | Task | Scope | Size |
|---|---|---|---|
| M2.1 | Git runner (no shell), ownership ledger event in core (`Resource` created/archived/removed), worktree create/remove under `~/.shprd/wt/<repo>/<run>/<task>` on branch `shprd/<run>/<task>`, and changed-paths-of-a-range checked against the task scope (the scope rules already live in `shprd-core::scope` from M1) | `shprd-git/src/{git,worktree,changed}.rs`, `shprd-core` events/state/rules | M |
| M2.3 | Integration: verify claimed commits, cherry-pick onto `shprd/<run>/int`, conflict → event, clean-tree guards | `shprd-git/src/integrate.rs` | M |
| M2.4 | Archive refs + bundles, `shprd restore`, local janitor (ledger-owned only, namespace-guarded) | `shprd-git/src/{archive,janitor}.rs` | M |

M2.2 was folded into M2.1: M1 already built the scope rules in `shprd-core::scope`.

Lanes: M2.1 first (the runner and ledger everything else uses), then M2.3 ∥ M2.4.

**Gate:** integration tests in temp repos: overlapping scopes refused, a commit
outside scope refused, conflict recorded and work retained, janitor never
touches a non-`shprd/*` branch, restore round-trips.

> Later change (M11): scope refusals were removed. A task's declared paths are a hint, no hand-in, review or landing is refused for paths, and a task touching a protected path waits for Accept. See [workflow](workflow.md).

---

## M3 · Roster

Goal: shprd knows which agents exist, can run, and have quota.

| ID | Task | Scope | Size |
|---|---|---|---|
| M3.0 | Harden `spec.rs` (from the M0 review): reject empty locations and misplaced `*`; reject an unmatched `{` in templates; validate launch templates at load (placeholder fits its field, `model_args` uses `{model}`); `load_dir` runs `validate` | `shprd-roster/src/spec.rs` | S |
| M3.1 | Detection: binary, version, sign-in probes per harness file; `shprd roster` | `shprd-roster/src/detect.rs` | M |
| M3.2 | Models: list per CLI where possible; `program/model` + effort parsing; `shprd set` | `shprd-roster/src/models.rs`, `shprd/src/cmd/set.rs` | S |
| M3.3 | Quota probes: Claude OAuth usage and Codex wham usage ported from og (`installer/og_quota.py:168-318`, credited); kilo profile; cache and 429 fallback | `shprd-roster/src/quota/` | M |
| M3.4 | Skills: discover global + native per CLI **in the launch environment**, compat list, folder-hash change detection | `shprd-roster/src/skills.rs`, `data/skills/compat.toml` | M |
| M3.5 | Clean launch environment builder (strip push credentials, drop og-style overrides) | `shprd-roster/src/env.rs` | S |
| M3.6 | `shprd doctor` | `shprd/src/cmd/doctor.rs` | S |

Lanes: M3.0 first (same file as later roster work); then M3.1 ∥ M3.3 ∥ M3.4 ∥ M3.5; then M3.2 ∥ M3.6.

**Gate:** `shprd roster` and `shprd doctor` on this machine correctly report the
five CLIs, their sign-in state, Claude and Codex quota, and which skills each CLI
sees.

---

## M4 · herdr adapter and `shprd up`

Goal: shprd can lay out a repo workspace and drive agents in herdr.

| ID | Task | Scope | Size |
|---|---|---|---|
| M4.1 | `Mux` trait with capability flags, plus a **fake mux** for tests | `shprd-mux/` | S |
| M4.2 | herdr adapter: workspace per repo, tabs, naming and renaming (★ ● ◇ ◌), split with ratio, run command, send prompt, read screen, agent-state events | `shprd-mux-herdr/` | L |
| M4.3 | `shprd up` / `shprd down`: preconditions, workspace reuse, orchestrator tab, resume | `shprd/src/cmd/{up,down}.rs` | M |
| M4.4 | `shprd agent spawn` / `shprd agent prompt`: worktree + tab + brief with skills | `shprd/src/cmd/agent/` | M |
| M4.5 | Keybind block writer for herdr (marked block; refuses to touch anything else) | `shprd-mux-herdr/src/keys.rs` | S |

Lanes: M4.1 first; then M4.2 (long, give it to a long-running worker) ∥ M4.5;
then M4.3 ∥ M4.4.

**Gate (manual, on herdr):** `shprd up` in a scratch repo opens
`shprd · <repo>` with `shprd·chat` (a placeholder for now) and
`★ <cli>·orchestrator`. `shprd agent spawn` opens `● <cli>·<task>` in its own
worktree. Running `shprd up` twice doesn't duplicate tabs.

---

## M5 · Watchdog and hand-off

Goal: no more silent stalls.

| ID | Task | Scope | Size |
|---|---|---|---|
| M5.1 | Record readers for claude, codex, opencode/kilo (SQLite), pi: provider errors + reset times | `shprd-watch/src/records/` | M |
| M5.2 | opencode/kilo `serve` status client: `GET /session/status`, `/event` SSE `session.status` retry | `shprd-watch/src/serve.rs` | M |
| M5.3 | Screen signatures + liveness sampling via the mux | `shprd-watch/src/screen.rs` | S |
| M5.4 | State machine working → suspect → dry → available; retry schedule; canary re-admit | `shprd-watch/src/machine.rs` | M |
| M5.5 | Hand-off: checkpoint commit, pick next worker, generated brief, same worktree, tab rename | `shprd-watch/src/handoff.rs` | M |

Lanes: M5.1 ∥ M5.2 ∥ M5.3; then M5.4; then M5.5.

**Gate:** with the fake mux, a scripted "agent goes silent" is marked dry within
5 minutes and handed off within 60 seconds of being marked dry. Manual check: a
real worker killed mid-task is detected and its task continues on the next
worker.

---

## M6 · Review and landing

Goal: work reaches `main` through a reviewed PR, and the run cleans up.

| ID | Task | Scope | Size |
|---|---|---|---|
| M6.1 | Reviewer dispatch: diff + acceptance as text, cross-vendor check, findings schema | `shprd/src/cmd/agent/review.rs`, `shprd-core/src/review.rs` | M |
| M6.2 | Gate rules: severities, `mustFix`, rounds, send-back as a new attempt | `shprd-core/src/gate.rs` | S |
| M6.3 | Full gate runner on the integration branch (repo-defined commands from `AGENTS.md` / config) | `shprd-git/src/gate.rs` | M |
| M6.4 | Landing: push, `gh pr create` with the generated body, merge policy (allow / ask / deny), `shprd land` | `shprd-git/src/land.rs`, `shprd/src/cmd/land.rs` | M |
| M6.5 | Remote janitor: delete merged `shprd/*` branches, close tabs, prune | `shprd-git/src/janitor.rs` | S |

Lanes: M6.1 ∥ M6.2 ∥ M6.3; then M6.4; then M6.5.

**Gate (★ dogfood):** shprd implements one real shprd task end to end on herdr:
plan, spawn, review, integrate, full gate, PR, your approval, land, cleanup.
From this point, shprd is built with shprd.

---

## M7 onward · the pivot (decided 2026-11-14)

M0–M6 built a command-line engine that runs in herdr tabs. From M7 the product
changes shape (see [Decisions](decisions.md#2026-11-14)):

- **Shepherdr is the design reference.** shprd re-implements Shepherdr's
  workflow and agent settings in Rust: a space per task, the coordinator, the
  tasks panel, the approve card, the agents settings page. Shepherdr is MIT;
  logic ported from it carries a file header and a `THIRD_PARTY_NOTICES.md`
  entry, like any borrowed code.
- **What shprd adds:** more agent CLIs (Pi, OpenCode and the rest), custom
  roles with their own system prompt, a dagr-like task graph, more tunnels
  (Cloudflare, ngrok, tunnl.gg, Tailscale) with a password login, and the
  engine's guarantees (ledger, engine-recorded review, integration gate).
- **Herdr desktop first, then the terminal hosts.** Herdr desktop gets a
  layout plus thin JS mods. herdr TUI gets a plugin, and tuios installs the same
  plugin (`tuios plugins install`, with `[herdr] manifest`). rex is planned
  later.
- **One Rust service, many thin UIs.** `shprd serve` holds all logic. The
  desktop mods, the TypeScript web app and the terminal panels are clients; the
  mods hold no logic because Herdr desktop only loads JavaScript mods.
- **No setup wizard.** Agent setup is a settings page, as in Shepherdr.

```
M7 study + spikes ─► M8 clean-up + service ─► M9 agents ─► M10 Herdr desktop ─┬─► M11 web + remote ─┐
                                                                                └─► M12 herdr TUI + tuios ┴─► M13 release ─► (later) rex
```

### Workspace changes

| Crate | Change |
|---|---|
| `shprd-core`, `shprd-git`, `shprd-views`, `shprd-watch` | keep |
| `shprd-roster` | becomes `shprd-agents`: Shepherdr-style roles plus custom roles, one adapter per agent CLI; detection, models, quota and launch env stay |
| `shprd-mux` | moves from tabs to Shepherdr's space-per-task model; `naming.rs` goes |
| `shprd-mux-herdr` | kept for the herdr TUI and tuios port; `keys.rs` goes (the plugin manifest declares keys); detects host capabilities at connect |
| `shprd-mux-hc` | **new**: Herdr desktop through herdr-core (`hc`) |
| `shprd-server` | **new**: axum HTTP + WebSocket API, login, tunnels, embedded web app |
| `shprd-tui` | emptied now; rebuilt in M12 as terminal versions of the desktop panels |
| `shprd-mux-tuios` | deleted: tuios speaks the herdr protocol |
| `shprd` | `up`, `launch`, `keys` deleted (tab-per-agent); `spawn` loses its tab handling; the other verbs stay and the service calls them |
| `web/` | **new**: Vite + React 19 + TypeScript, types generated from Rust with ts-rs |
| `host/` | **new**: the Herdr desktop layout and mods, `plugin/herdr-plugin.toml`, `plugin/tuios-plugin.toml` |

Stack as built through M11: tokio, axum, tower-http, ts-rs, rust-embed, argon2,
qrcode, tracing on the Rust side (the rate limits are shprd's own, in
`shprd-remote`); Vite, React 19, xterm.js, marked + DOMPurify and Vitest on the
web side. ratatui + crossterm for `shprd-tui` is still the M12 plan.

---

## M7 · Study and spikes

| ID | Task | Scope | Size |
|---|---|---|---|
| M7.1 | Shepherdr workflow and service spec | report only | M |
| M7.2 | Shepherdr desktop UI spec (layout, mods, how mods reach the service, visual tokens) | report only | M |
| M7.3 | Shepherdr agents and settings spec, adapter and custom-role design | report only | M |
| M7.4 | Shepherdr web app and remote access spec, paddock comparison, login threat model | report only | M |
| M7.5 | Spike: drive `hc` against a real Herdr desktop | report only | S |
| M7.6 | Spike: a mod calling a local service (fetch to 127.0.0.1 with a token) on a real Herdr desktop | report only | S |
| M7.7 | Spike: `tuios plugins install` of a test plugin whose `tuios-plugin.toml` points at a `herdr-plugin.toml` | report only | S |
| M7.8 | This plan updated from M7.1–M7.7: task lists for M8–M10 | `docs/` | S |

Lanes: M7.1 ∥ M7.2 ∥ M7.3 ∥ M7.4 ∥ M7.7; M7.5 ∥ M7.6 once Herdr desktop is
installed; then M7.8.

**Gate:** the four specs and three spike results exist, and the M8–M10 task
lists are approved.

**Done 2026-11-14** (board tasks SHP-11..16, all passed review). Results that
shaped M8–M10: the mod runtime has `fetch` but no streaming (no WebSocket, SSE
or stream reader), so mods use long-poll plus a watched file; `hc` is
JSON-RPC 2.0 with an event stream and the window's layout in the `herdr-app`
document; one plugin folder serves herdr TUI and tuios, with commands only in
`herdr-plugin.toml` and `tuios-plugin.toml` holding `schema = 1`, id, grants
and `[herdr] manifest`; on tuios, creating panes needs the `admin` grant, no
`worktree.created` event fires, and `worktree.create` refuses `path`.

---

## M8 · Clean-up and service core

| ID | Task | Scope | Size |
|---|---|---|---|
| M8.1 | Retire tab-per-agent: delete `cmd/{up,launch,keys}.rs`, `shprd-mux/naming.rs`, `shprd-mux-herdr/keys.rs`, the `shprd-mux-tuios` crate and `shprd-tui`'s stub; fix every caller and test | `crates/shprd`, `shprd-mux`, `shprd-mux-herdr`, `Cargo.toml` | M |
| M8.2 | Fixes from the M0–M6 assessment: run lock on `approve`, watchdog treats idle as unhealthy only with evidence, canary reports real success, opencode test uses a scratch HOME, live tests use short socket paths | `shprd/src/cmd/{approve,run_lock}.rs`, `shprd-watch`, `shprd-roster` tests | M |
| M8.3 | Per-task landing: one PR per task from its branch, `MergePolicy{Ask, Checks, Auto}`, already-merged and squash-merged detection, nothing-to-merge, branch from the fresh remote base | `shprd-git/{land,forge,worktree}.rs`, `shprd-core/config.rs` | L |
| M8.4 | Agent-callable verbs that append under the ledger lock (`shprd task status / ready / …`); `ready` refuses without the worker's checks report (which gates ran, their result) and an engine-recorded cross-vendor review | `shprd-core/{event,rules}.rs`, `shprd/src/cmd/` | M |
| M8.5 | Derived status: port Shepherdr's ordered status rules (8 statuses from stored state + live agent state) as a pure function with fixtures (MIT header + notice) | `shprd-views/status.rs` | M |
| M8.6 | `shprd-server` + `shprd serve`: axum on 127.0.0.1, `service.json {port, id}` + token file + `/api/health` handshake, `GET /api/state`, long-poll `/api/changes?since=<seq>`, `POST /api/action`, WebSocket for the web app, ts-rs types into `web/src/gen/` | new crate, workspace deps | L |
| M8.7 | The tick: an idempotent "advance the world" pass in the service (queue starts, `--after` waits, merge-policy checks) | `shprd-server`, `shprd-core` | M |

Lanes: M8.1 alone first; then M8.2 ∥ M8.3 ∥ M8.4 ∥ M8.5 ∥ M8.6; then M8.7.

**Gate:** the existing gates pass after the clean-up, and with the fake host a
task is added, delegated, handed in with its checks, reviewed and landed as its
own PR through the API.

---

## M9 · Agents

| ID | Task | Scope | Size |
|---|---|---|---|
| M9.1 | `shprd-roster` → `shprd-agents`; harness schema gains permission modes (Shepherdr's set), model catalog source, brief delivery (first message or system-prompt file), status source (OSC 7501, hook, screen), ask-hook installer | `crates/shprd-agents`, `data/harness/` | M |
| M9.2 | Adapters with verified harness files for Claude, Codex, Pi, OpenCode (one real launch each, `#[ignore]` tests) | `shprd-agents`, `data/harness/` | M |
| M9.3 | Settings model: roles as a keyed table (coordinator, worker, reviewer, backup) plus custom roles, each `{program, model, effort, fast, perm, fallback, template}`; layers everywhere > repo > task; reviewer vendor must differ from the worker's | `shprd-agents`, `shprd-core/config.rs` | M |
| M9.4 | Role templates as editable files (Shepherdr's structure, credited) and a brief with a Settings block that says where each value came from | `shprd-views/brief.rs`, `data/templates/` | M |
| M9.5 | Needs-you cards: question / permission / plan asks in a file queue, per-CLI hooks (Claude and Codex first, Pi and OpenCode where they allow), answer API in the service | `shprd-core`, `shprd-agents`, `shprd-server` | L |
| M9.6 | Fallback ladder: provider-error classes, nudges at 1/2/4/8 min, one backup-agent handover per task with the chat tail | `shprd-watch` | M |
| M9.7 | News to the coordinator: derived-status diffs, a settle window, one message per space, re-checked before sending | `shprd-server`, `shprd-views` | M |

Lanes: M9.1 first; then M9.2 ∥ M9.3 ∥ M9.5 ∥ M9.6; then M9.4 ∥ M9.7.

**Gate:** one real task runs end to end with Pi as the worker and a reviewer
from another vendor, including one needs-you card answered through the API.

**Gate result (2026-11-15): passed** on the third live run, under `shprd serve` alone:
Pi (DeepSeek) worked, Pi (GLM) reviewed, the worker raised its own card with
`shprd ask raise`, the person answered over `POST /api/asks/answer`, and the tick
landed the task on `integration/m9` under `tasks.merge = "auto"` with no other
intervention. The runs found gaps that fixes A–K closed before the milestone PR
(send-back after a failed review, asks in the review packet, visible tick failures,
the workspace label). Known limit: Codex's live-launch test is skipped while the
account is out of quota.

---

## M10 · Herdr desktop

| ID | Task | Scope | Size |
|---|---|---|---|
| M10.1 | `shprd-mux-hc`: framed JSON-RPC over `hc connect`, version gate, reconnect, event fold with `missed`/reset re-reads, placement by `herdr.space`/`herdr.tab` labels, OSC 7501 parsing (fixture for the `:` grammar) | new crate | L |
| M10.2 | Host trait reshaped around spawn / write / keys / read / wait / kill / subscribe / doc / exec and a space per task; fake host updated; `shprd-mux-herdr` maps tuios's `forbidden` | `shprd-mux`, `shprd-mux-herdr` | M |
| M10.3 | `host/herdr-desktop/`: `layouts/shprd.json` on Shepherdr's skeleton, a shared UI-token module, mod-to-service client (long-poll + watched change file), `shprd install herdr-desktop` that owns only its files and merges keys | `host/herdr-desktop/`, `shprd/src/cmd/` | M |
| M10.4 | Mods A: spaces sidebar, tab strip, status bar, toasts | `host/herdr-desktop/mods/` | L |
| M10.5 | Mods B: tasks panel, accept card (changes, checks, review), task PR | `host/herdr-desktop/mods/` | L |
| M10.6 | Mods C: settings (agents, roles, custom roles, templates), coordinator, needs-you cards | `host/herdr-desktop/mods/` | L |
| M10.7 | Graph: dagr in a terminal block, refreshed on every ledger write | `shprd-views`, layout | S |
| M10.8 | Coordinator chat: transcript readers for Claude, Codex, Pi, OpenCode and the chat mod | `shprd-agents`, `host/herdr-desktop/mods/` | L |

Lanes: M10.1 ∥ M10.2 ∥ M10.3; then M10.4 ∥ M10.5 ∥ M10.6 ∥ M10.7 ∥ M10.8.
Every mod passes `herdr-app check` and is screenshotted in a private window
(`HC_SESSION` + `HERDR_APP_HOME`) before review.

**Gate:** on Herdr desktop, shprd works the way Shepherdr does, and also runs
Pi and OpenCode agents.

**Gate result (2026-11-16): passed after fixes.** The live run (a private Herdr
window and `herdr-core`, `HC_SESSION` + `HERDR_APP_HOME`) came back *pass with gaps*:
`shprd serve` alone drove a coordinator and three tasks (one after another, one
raising a needs-you card) to landing, each in its own space, with the graph redrawn
on every ledger write. It found nine bugs; fixes N–R (#64–#68) closed them: a
stopped task no longer wedges, `tasks.tick_secs` applies live, a crashed `serve`
no longer blocks the next, the chat reads real transcripts (Claude's slug rule,
the program's own home), OpenCode gets its brief once its ready line shows, mods
follow a restarted service, `install` takes the binary's own tree and restores a
pruned layout, the accept card diffs against the integration branch, and the
coordinator gets the news when the person started it. Pi and OpenCode both ran
end to end. Left for later: a per-task thread view (chat, terminal, files) like
Shepherdr's, and `shprd doctor` reading a crashing CLI as "signed out".

---

## M11 · Web app and remote access

The web app (board, graph, terminal, settings; phone and desktop layouts) and
the tunnels (Cloudflare quick and named, ngrok, tunnl.gg, Tailscale serve) with
a password login (argon2, session cookie, rate limiting, origin checks), QR
code in the UI.

Spec: `_research/m7/M7d-web-remote.md` §4 (routes, tunnels, the login and its
threat table). The web app is served by `shprd serve` from the same origin; the
login and the tunnels live in one new crate, `shprd-remote`, and the router stays
in `shprd-server`.

| ID | Task | Scope | Size |
|---|---|---|---|
| M11.1 | The login: `shprd web password set` (argon2id, local only, length and common-password checks), server-side sessions with a `__Host-` cookie, `POST /api/login`, `/api/logout`, `GET /api/session`, rate limits (M7d §4.4 table), a `Host` allowlist (unknown host → 421), `Origin` + `x-shprd` on writes and socket upgrades, a fresh password for destructive actions, `shprd web sessions` to list and revoke. The loopback token door stays for the mods | new crate `shprd-remote`, `shprd-server`, `shprd/src/cmd/` | L |
| M11.2 | Tunnels: a provider trait with Tailscale serve, cloudflared quick and named, ngrok and tunnl.gg; `shprd web up --tunnel <p>`, `down`, `status`; `<state>/web.json`; a public tunnel refuses to start without a password; the active tunnel's host joins the allowlist; the QR code (terminal half-blocks and SVG); the tunnel stops with the service | `shprd-remote`, `shprd/src/cmd/` | L |
| M11.3 | Web app scaffold: Vite + React + TypeScript in `web/`, one `client.ts` (`x-shprd`, same-origin credentials), the generated `ts-rs` types, the login screen, phone (< 800 px) and desktop layouts, built assets embedded in the binary and served with an SPA fallback; CI runs `tsc --noEmit`, the web build and a diff check of the generated types | `web/`, `shprd-server`, `.github/` | M |
| M11.4 | Web screens A: tasks board, accept card (changes, checks, review), needs-you cards, toasts | `web/` | L |
| M11.5 | Web screens B: the graph, drawn from the run document the dagr block shows | `web/`, `shprd-server` | M |
| M11.6 | Web screens C: terminal (xterm.js over a socket; watch by default, type on demand, OSC 52 stripped) and the coordinator chat | `web/`, `shprd-server` | L |
| M11.7 | Web screens D: settings (agents, roles, prompts) and Settings › Remote (tunnel, sessions, QR) | `web/` | M |

Lanes: M11.1 ∥ M11.3; then M11.2 (after M11.1) ∥ M11.4 ∥ M11.5 ∥ M11.6 ∥ M11.7
(after M11.3). Every screen is screenshotted at 390 px and 1440 px before review.

**Desktop parity fixes (added 2026-11-16).** An audit of the M10 window against
Shepherdr's source (report kept outside the repo)
found the desktop departing from Shepherdr's shapes: settings in a panel tab
instead of a dialog, no "+" menu, the accept card inline, no search or filters in
Tasks, no chat thread per task. Shepherdr's window is the reference, so these land
in M11 before the web screens copy them:

| ID | Task |
|---|---|
| fix S | Settings as Shepherdr's frameless dialog with a page list |
| U1 | Tab bar: "+" menu (Terminal, New task, Start an agent), tab menu, pills |
| U2 | Tasks panel: search, filter row, folding groups |
| U3 | Accept card as a 640×640 dialog; thread head with Review and the view switch |
| U4 | The per-task chat thread (question cards in the timeline; the Asks tab goes) and Chat │ Terminal on agent panes; the sidebar coordinator block goes |
| U5 | Sidebar: one-line rows, right-click menu |
| U6 | Keys, Jump (Ctrl+K), Keybinds |
| U7 | The graph gets its own panel |
| U8 | Look: icons, status glyphs, pane header, foot, status bar, toasts |

The per-task thread, deferred at M10, is built here. M11.4 waits for U2–U4 and
M11.6 for U4.

**Audit and its fixes (SHPR-17 on).** An audit of the desktop layout
(SHPR-17) found more departures from Shepherdr, and a second round of tasks fixed
them. Pull request numbers are on `integration/m11`.

| Group | Tasks |
|---|---|
| Live checks stay private | SHPR-1 (#76): live tests never reach the real herdr; SHPR-20 (#91): `shprd` keys work on the installed layout |
| Prototype and tidy | SHPR-18 (#90): spaces tree, Agents pane, Tasks and Graph; SHPR-19 (#94): mods stop leaking timers; SHPR-21 (#92): web app tidy and a release build that embeds it |
| Agents and spaces | SHPR-23 (#96): agents run headless, one space per repo; SHPR-25 (#97): New space, New task, spaces tree, Agents pane; #104: sidebar spaces carry Herdr's real space id, name and branch |
| Settings | SHPR-22 (#95): the settings dialog completed; SHPR-24 (#98): settings that saved but did nothing now take effect |
| Tabs, Tasks, thread | SHPR-26 (#99): tab bar, menus and Jump polish; SHPR-27 (#100): Tasks panel deps, resend, filter, group by; SHPR-28 (#101): task thread and Accept, end to end; SHPR-29 (#102): Tasks and Graph in one panel (#103 regenerated the `Action` binding) |
| Coordinator | SHPR-33 (#105): the coordinator as the space's Assistant chat |

The audit's decisions are in [decisions.md](decisions.md#2026-11-18). In short:
agents follow Shepherdr v3.1.34 (no tab or space per task; labelled `hc` children
with a real terminal; a task's thread switches between Chat and Terminal); Tasks
and the graph are one panel; the Spaces sidebar is a tree of live tasks with an
Agents pane below; the coordinator is the space's Assistant chat tab.

**Desktop work, SHPR-49 to SHPR-88.** After the gate, the work moved to making the
shprd layout work with zero commands. Pull request numbers are on `integration/m11`.

| ID | Task | Scope | Size |
|---|---|---|---|
| SHPR-49 | Install takes the layout from the binary's own tree (#120) | `shprd/src/cmd/`, `host/herdr-desktop` | S |
| SHPR-50, 51, 52 | Accept with a short SHA and refused actions say why (#121); review diffs from the attempt's own base (#122); install refuses the real herdr-app under an agent (#123) | `shprd-server`, `shprd-core`, `shprd/src/cmd/` | M |
| SHPR-57, 59 | Worker questions and permissions become cards (#126); cards that act: take as is, refused hand-in, stale asks, dead reviewer (#130) | `shprd-core`, `host/herdr-desktop` | M |
| SHPR-58 | Second attempts keep their commits; Retry without raw git (#134) | `shprd-core`, `shprd-git` | M |
| SHPR-62, 63 | Proof on the Accept card and in the review packet (#127); Tasks pane minors and the question-card focus bug (#129) | `shprd-core`, `host/herdr-desktop` | M |
| SHPR-61 | A fresh repo can add tasks; a serve restart keeps the space and chat (#131) | `shprd-server`, `host/herdr-desktop` | M |
| SHPR-65, 66, 69 | Reused repo space (real name, one live coordinator, own agents only, #135); the layout's Assistant is shprd's coordinator (#136); chat never talks to Shepherdr's Assistant (#138) | `shprd-server`, `host/herdr-desktop` | M |
| SHPR-67 | Old tasks never block a fresh start: fold Done, clear stale in one action (#137) | `shprd-core`, `host/herdr-desktop` | S |
| SHPR-68, 70 | Click a repo space and it works with no manual `shprd serve`, several repos at once (#139); add a folder in the layout and chat with zero commands (#140) | `shprd-server`, `host/herdr-desktop` | L |
| SHPR-71, 72 | The coordinator makes tasks and never does the work, no Claude trust prompt (#141); adding a folder starts nothing (#142) | `shprd-server`, `host/herdr-desktop` | M |
| SHPR-73 | Chat renders like Shepherdr: folded tool calls, no raw `hc` tags, line breaks kept (#145) | `host/herdr-desktop` | S |
| SHPR-75 | Sidebar spaces stable, toggle reliable; a new space opens on the Assistant (#143) | `host/herdr-desktop` | S |
| SHPR-76 | Accept merges locally when the repo has no GitHub remote; plain land errors (#146) | `shprd-git`, `shprd-server` | M |
| SHPR-77 | One shprd service for every repo; switching spaces is instant (#147) | `shprd-server`, `shprd/src/cmd/`, `host/herdr-desktop` | L |
| SHPR-79, 80 | Chat is always shprd's coordinator and tasks always reach the panes, proven end to end (#149); a re-added folder never hangs on Loading (#150) | `shprd-server`, `host/herdr-desktop` | M |
| SHPR-81 | Task actions update the panes at once, rolled back on error; Graph states right-aligned (#152) | `host/herdr-desktop` | S |
| SHPR-83 | Accept works with a real click; coordinator escalations true and command-free; held tasks say why (#155) | `shprd-core`, `host/herdr-desktop` | L |
| SHPR-84 | Chat composer like Shepherdr's: Enter sends, model picker, voice, attachments the coordinator can read (#153) | `shprd-server`, `host/herdr-desktop` | L |
| SHPR-85 | Toasts draw one card with a divider between messages (#154) | `host/herdr-desktop` | S |
| SHPR-86 | Agents never stop on a trust prompt; closing a space stops its agents (#156) | `shprd-agent`, `shprd-server`, `host/herdr-desktop` | M |
| SHPR-87 | A reviewer out of quota hands the review to the next reviewer; the reviewer comes from Settings (#157) | `shprd-core`, `shprd-server` | M |
| SHPR-88 | Switching an existing space to the shprd layout starts shprd's coordinator and shows it in the chat (#158) | `shprd-server`, `host/herdr-desktop` | M |

SHPR-36 to SHPR-48 (gate fixes and Settings rounds) are in the gate result below.

**Gate:** the M10 flow runs from a desktop browser and from a phone over each
tunnel, and every request without a session is refused.

**Gate result (2026-11-19): passed with gaps.** The live gate over ngrok came back
*pass with gaps*. Fixes A–E landed from it: A (#110) the login limit per client,
reauth on accept and the health id; B (#107) the web app at 390 px, the QR following
the mask and dropped tasks in the graph; C (#108) typing in the web terminal on
Herdr desktop; D (#109) agents run the service's own `shprd`; E (#111) the
coordinator record per repo and space. Settings rounds 1–7 (#112–#118) then
reworked Settings to match Shepherdr: program tabs from the detected harnesses,
renamable roles, and a redesigned Remote page. Tail fixes landed after the gate:
`serve` finds its Herdr space by shprd's own label (#119), `install` takes the
binary's own layout (#120), Accept works with a short SHA and a refused action says
why on the card (#121), review diffs from the attempt's own base (#122), and
`install` refuses the real herdr-app when an agent starts it (#123). #107, #109
and #110 merged without a passing cross-vendor review.

Parked gaps:

- G1: a forged `X-Forwarded-For` header gets past the per-client login limit over
  ngrok (the account limit of 30/h and the global limit of 60/h still hold).
- G2: an idle coordinator web terminal returns 409.
- G3: the desktop graph omits dropped tasks.
- G4: a landed task's accept card says no changes.
- G5: the coordinator can stay stuck at "blocked (permission)".

**Gate result (2026-11-22).** The integration gate passed on `integration/m11`
head 2bd4cd4: `cargo fmt --all --check`, clippy, and the tests (2384 passed, 21
ignored). CI is green.

Known gaps:

- #107, #109 and #110 merged without a passing cross-vendor review.
- G1 to G5 above are still open.
- SHPR-61, 71, 72, 75 and 76 have gaps noted in their pull requests.
- SHPR-81: the live check had no real agents.
- SHPR-83: all five tasks never reached Done in one run, the `ops.py` conflict case
  never came up live, and Retry review is covered by tests only.
- SHPR-84: real OS key presses are not proven, and the mic is unproven (it needs
  the person's ElevenLabs key).
- SHPR-85: the only independent review was GLM, after codex returned nothing; there
  is no light-theme screenshot.
- SHPR-86: round 2 was recorded before its last fixes, and the bypass warning is
  not a known blocking prompt.
- SHPR-87: the "no reviewer left" card is tested, not seen live. Reviewers ignore a
  program line's read-only mode, because pi's read-only tools cannot write the
  findings file.
- Several reviews ran on GLM only because codex was out of quota.
- The doctor tests read `SHPRD_REPO` from the environment (fixed by task
  `m11-doctor-env`).

---

## M12 · herdr TUI and tuios

`plugin/herdr-plugin.toml` (actions, panes, events) and
`plugin/tuios-plugin.toml`; `shprd-tui` panels that mirror the desktop mods as
closely as a terminal allows; capability checks in `shprd-mux-herdr` with a
polling fallback where tuios lacks `terminal.attach`.

**Gate:** the M10 flow passes on herdr TUI and on tuios.

---

## M13 · v0.1.0

Release binaries with the web app embedded, Homebrew tap, install docs per
host, a clean-machine run on each host, `THIRD_PARTY_NOTICES.md` audit.

### Later: rex

A Lua action that opens `shprd-tui` in a block, once rex is public; native
blocks when rex publishes its block-plugin API.

---

### Releases after v0.1.0

From v0.1.0 on, **every PR merged into `main` produces a tagged, published
release.** No release PRs and no manual tagging.

- **Trigger:** a GitHub Actions workflow on `push` to `main`. Merging an
  approved PR is the only way anything reaches `main`.
- **Next version** comes from the Conventional Commits in the merge since the
  last tag:

  | Commits contain | Before 1.0 | From 1.0 |
  |---|---|---|
  | `feat!` or `BREAKING CHANGE` | minor | major |
  | `feat` | minor | minor |
  | `fix`, `perf` | patch | patch |
  | only `docs`, `chore`, `ci`, `test`, `refactor` | patch | patch |

  Every merge releases, so a docs-only PR still ships a patch. Its notes say
  "no functional change".
- **Tag, don't commit.** The workflow creates and pushes tag `vX.Y.Z` and does
  **not** push a version-bump commit. That way protected `main` never needs a
  bot that bypasses the PR rule. The build stamps the version from the tag
  (`SHPRD_VERSION` env read in `build.rs`, falling back to `Cargo.toml`), so
  `shprd --version` matches the release. `Cargo.toml` stays at the last minor
  version and is bumped by hand only when publishing to crates.io.
- **Publish:** build release binaries (macOS arm64 + x86_64, Linux x86_64 +
  arm64), attach them with checksums to a GitHub Release, and write notes
  grouped by commit type with each merged PR linked. Then update the Homebrew
  tap formula.
- **Safety:** the workflow refuses to tag if the CI run for that commit didn't
  pass, or if the tag already exists. A failed publish leaves no tag behind.
- **Before v0.1.0:** no automatic releases. Milestone merges stay untagged
  until M13 ships v0.1.0.

---

## How each milestone runs

1. **Kickoff.** I post the milestone's task list with scopes and acceptance
   criteria. You approve or adjust it.
2. **Fan-out.** Tasks in the same lane step go out in parallel, one worktree
   each, run from the work board. Workers are Pi (DeepSeek); the coordinator
   only plans, integrates and reports.
3. **Integrate.** Finished tasks are combined on one integration branch, and
   the integration gate runs there once.
4. **Review.** The combined diff goes to a reviewer from a different vendor
   than the workers (today GLM, reviewing DeepSeek workers) with the
   acceptance criteria.
5. **PR.** One PR per milestone step, with review findings in the body. You
   approve the merge.
6. **Milestone gate.** The demo above is run and recorded in
   [Decisions](decisions.md) before the next milestone starts.

## Open decisions before M0 starts

1. **Branch policy:** I propose `main` protected, with every change landing by
   PR. Integration branches are `integration/m<N>` and task branches `feature/<task>`. You merge.
2. **CI platforms:** macOS + Linux. Windows (WSL only) deferred.
3. **Minimum Rust version:** stable at M0 time, pinned in `rust-toolchain.toml`.
4. **TUI library:** ratatui + crossterm (proposed).
