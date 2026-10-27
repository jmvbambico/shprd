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

### How quota and retries are read (verified 2026-10-27)

| CLI | Source |
|---|---|
| claude | `GET https://api.anthropic.com/api/oauth/usage`, bearer token from Claude Code's own Keychain credential; `five_hour` / `seven_day` with `utilization` or `used_percentage` and `resets_at`. Ported from og (`installer/og_quota.py:168-211`). |
| codex | `GET https://chatgpt.com/backend-api/wham/usage`, token and account id from `~/.codex/auth.json`; `rate_limit.primary_window` / `secondary_window`, `reset_at`, `credits`. Ported from og (`og_quota.py:252-318`). |
| kilo | `kilo profile --json` balance; on the free router the balance doesn't bind. |
| opencode, kilo | `opencode serve` / `kilo serve`: `GET /session/status` and the `/event` SSE stream carry `SessionStatus {type: "retry", attempt, message, next}` while a turn is being retried. Also `session.next.retried` and `session.error` with `statusCode` and `isRetryable`. |
| pi | `--mode json` / `--mode rpc` emit `auto_retry_start {attempt, maxAttempts, delayMs, errorMessage}` and `auto_retry_end`. No quota. |

## Harness table

One TOML file per CLI in `data/harness/`, loaded and validated by
`shprd-roster::spec`:

- **detect:** binary name, version arguments, sign-in hint.
- **launch:** the interactive command, plus **argument templates**:
  `model_args`, `effort_args`, `system_prompt_args`, `resume_args`,
  `auto_approve`. Each element is one argv token with at most one placeholder:
  `{model}`, `{effort}`, `{prompt}`, `{prompt_file}` or `{session}`. For example,
  Codex's effort is `["-c", "model_reasoning_effort={effort}"]`.
  `render_args` fills them in.
- **models:** the command that lists them.
- **records:** format (`jsonl` or `sqlite`) and a location pattern. The
  location may use `~` (first component only), `{cwd_slug}`, `{session}`,
  `{yyyy}`, `{mm}`, `{dd}`, and `*` within one component. For example,
  `~/.codex/sessions/{yyyy}/{mm}/{dd}/rollout-*.jsonl`.
- **quota:** source (`anthropic_oauth`, `codex_wham`, `kilo_profile` or
  `none`) and the live retry signal, if any.
- **skills:** `home_dirs` (must start with `~/`) and `project_dirs` (relative
  to the repo root, e.g. Codex's `.agents/skills`).
- **dry_patterns:** on-screen strings that mean "rate limited" or "out of
  quota".
- `launch.verified` and `records.verified` say whether those facts were
  checked on a real machine.

The starting data comes from herdr-projects' per-CLI table (MIT, credited in the
README) and is re-checked against each CLI version we support.

## Launch environment

Agents are started with a clean, known environment, never the parent's as-is.
For example, a parent that sets `OPENCODE_DISABLE_EXTERNAL_SKILLS=1` would hide
every global skill from OpenCode. `shprd doctor` checks what each agent actually
sees in the environment shprd will launch it with.
