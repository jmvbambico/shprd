# Agents

Status: design; facts below were checked on one machine on 2026-10-27.

## Supported CLIs (first release)

| CLI | Version checked | Conversation record | Provider errors recorded |
|---|---|---|---|
| Claude Code `claude` | 2.1.311 | `~/.claude/projects/<cwd-slug>/<session>.jsonl` | yes: `isApiErrorMessage: true`, `error: "rate_limit"`, text with the reset time |
| Codex `codex` | 0.164.0 | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | yes, plus quota on every turn (below) |
| OpenCode `opencode` | 1.18.35 | `~/.local/share/opencode/opencode.db` (SQLite: `session`, `message`, `part`, `event`) | final errors in `message.data.error`; **retries are not persisted** |
| Kilo `kilo` | 7.7.3 | `~/.local/share/kilo/kilo.db` (same schema; Kilo is an OpenCode fork) | final errors in `message.data.error`; retries not persisted |
| Pi `pi` | 1.4.0 | `~/.pi/agent/sessions/<cwd-slug>/<ts>_<id>.jsonl` | `stopReason: "error"` plus `errorMessage`; no reset time |

The record serves two jobs:

1. **`shprd·chat`** draws the orchestrator's conversation from it.
2. **The watchdog** reads provider errors from it.

Each CLI gets a small reader. Locations and formats are kept as data in the
harness table, so supporting a new CLI version is a table change.

## Quota visibility

| CLI | Before dispatch | After failure |
|---|---|---|
| codex | yes: `event_msg/token_count.rate_limits.primary{used_percent, window_minutes, resets_at}` | yes |
| claude | yes: the OAuth usage windows `five_hour` and `seven_day` with `resets_at` (og's `og stats` reads these) | yes, with reset time |
| opencode | no (`opencode stats` is local token/cost accounting) | final error only |
| kilo | no | final error only |
| pi | no | error message only |

For CLIs that can't report quota up front, the screen-and-silence watchdog does
the work. See [Availability](availability.md).

## Harness table

One TOML table per CLI, shipped with shprd:

- detection: binary name, version probe, sign-in probe;
- launch: interactive command, model flag, effort flag, auto-approve flag,
  system-prompt injection, resume flag;
- models: how to list them;
- records: conversation location and reader;
- dry patterns: on-screen strings that mean "rate limited" or "out of quota";
- skills: which global and native skill folders the CLI reads.

The starting data comes from herdr-projects' per-CLI table (MIT, credited in the
README) and is re-checked against each CLI version we support.

## Launch environment

Agents are started with a clean, known environment, never the parent's as-is.
For example, a parent that sets `OPENCODE_DISABLE_EXTERNAL_SKILLS=1` would hide
every global skill from OpenCode. `shprd doctor` checks what each agent actually
sees in the environment shprd will launch it with.
