# Commands

Status: design.

## For you

| Command | Does |
|---|---|
| `shprd setup` | the setup wizard (TUI): agents, models, skills, remote, layout |
| `shprd up` | start or resume this repo's run |
| `shprd down` | finish the run; keeps branches until landed |
| `shprd` / `shprd status` | tasks, agents, quota, and what needs you |
| `shprd brief` | a short, paste-able summary of the run |
| `shprd remote [--invite]` | remote access status; `--invite` shows a fresh pairing QR |
| `shprd land` | land approved work now (normally automatic) |
| `shprd doctor` | check CLIs, sign-ins, skill visibility, mux, relay, git and gh |
| `shprd set orchestrator\|reviewer <agent> [--effort E]`, `shprd set workers <agent>…` | change the roster in your global config, or the repo's with `--repo <path>`; e.g. `shprd set reviewer codex/gpt-6-luna --effort high` |
| `shprd roster` | show agents, roles, models, availability |
| `shprd restore <task>` | bring back archived work |

Every command supports `--json`. `shprd completion zsh` prints shell completion.

Agent and model strings use the CLI's own model names: `program/model`, e.g.
`opencode/deepseek-v4.1-flash-free` or `claude/claude-opus-5-5`, with effort set
separately (`--effort high`). Pi's models must name their provider, because the
same model id can be served by several providers:
`pi/Command Code/deepseek/deepseek-v4.1-flash`.

### How `shprd set` writes your config

- The change is validated in memory first; an invalid change writes nothing and
  exits 2 (for example a reviewer equal to the orchestrator).
- Comments and other keys in the file are kept.
- The new file is written to a temporary file, synced, and renamed into place, so
  the config is never half-written.
- Two `shprd set` runs never interleave: they share a lock file
  (`.config.toml.lock` next to the config). A second run while one is active
  exits 2.
- If the file changes while `set` is working, `set` refuses (exit 2) instead of
  overwriting. The one gap: an editor that ignores the lock file and saves in the
  instant between that final check and the rename can still be overwritten. No
  POSIX tool can close that window; editors such as vim have the same one.

## For agents

Hidden from the main help. The orchestrator and workers call these, and the
engine validates every one:

| Command | Who | Does |
|---|---|---|
| `shprd agent task add` | orchestrator | define a task: title, scope, acceptance, tags, skills |
| `shprd agent spawn <task>` | orchestrator | start a task on the next available worker |
| `shprd agent prompt <task>` | orchestrator | send a follow-up to a task's agent |
| `shprd agent report` | worker | submit status, tests, changed paths, commit, skills used |
| `shprd agent gate pass\|fail <task>` | engine / reviewer | record a review outcome with findings |
| `shprd agent escalate` | any | non-blocking note to the orchestrator, always with a stated default |
