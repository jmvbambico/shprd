# Architecture

Status: design. Language: Rust.

## Three layers

```
┌───────────────────────────────────────────────────────────────────────┐
│ CORE (host-agnostic, most of the code)                                │
│  engine: event log, fold, transition rules, scheduler, gate runner    │
│  contract: AGENTS.md + .shprd/config.toml                             │
│  roster: CLI discovery, harness table, models, fallback, quota        │
│  git: plain-git worktrees, integration branch, cherry-pick, scopes    │
│  review routing, landing via gh, janitor                              │
│  views: dagr run.json, brief, status, PR body                         │
├───────────────────────────────────────────────────────────────────────┤
│ HOST ADAPTER (one per multiplexer, thin, capability-negotiated)       │
│  spawn agent in pane · send prompt · read screen · agent-state events │
│  layout · tab/window naming · notifications · keybind install         │
│  optional: pane grants, human ask/answer, resumable event stream      │
├───────────────────────────────────────────────────────────────────────┤
│ REMOTE (host-specific, not abstracted)                                │
│  herdr → herdr-mobile-relay       tuios → tuios-web / SSH / push      │
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

## The chat pane and the orchestrator

The orchestrator is a **real CLI in its own tab** (`★ claude·orchestrator`).
`shprd·chat` is a separate, non-agent pane that:

- reads the orchestrator CLI's own conversation file and draws it as a
  transcript, interleaved with event cards from the log;
- sends what you type to the orchestrator through the multiplexer
  (`herdr agent prompt` or tuios `queue-prompt`).

The phone, the multiplexer and the chat therefore all see the same session.
See [Agents](agents.md) for where each CLI keeps its conversation.

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
