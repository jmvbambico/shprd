# Commands

Status: built through M9. `shprd up`, `shprd down`, `shprd remote` and `shprd keys` are
described below but not built yet; the setup wizard was dropped in the M7 re-plan.
Today a run starts with `shprd run start` and `shprd serve` drives it.

## For you

| Command | Does |
|---|---|
| `shprd up` | start or resume this repo's run |
| `shprd down` | finish the run; refuses (exit 2, nothing written) while any task is unfinished or the run's pull request has not merged, and names each task and how to proceed: `shprd up` restarts the orchestrator or watchdog and resumes this run. `shprd down --abandon` (you only, never an agent or the orchestrator) asks you to confirm at a terminal, naming the run and each task it would leave with its branch; a 'no' or a missing terminal writes nothing and closes nothing. On a 'yes' it ends the run anyway, records which tasks were left (with an unmerged pull request, the integrated tasks it carried too), and keeps their branches. The orchestrator may end a run normally once every task is integrated and the pull request merged |
| `shprd run start --run-id <ID> --base <BRANCH> --integration <BRANCH>` / `shprd run finish` | start a run (the first event in the log) / finish it. The `--integration` branch must already exist (`git branch integration/m<N> main`); `run start` does not create it yet |
| `shprd serve [--port N]` | the service: a loopback HTTP API for every UI, plus the tick every `tasks.tick_secs`. See [The service](#the-service) |
| `shprd tick [--once] [--json]` | run one pass of the service's tick by hand: start, review, verdict, send back, escalate, land |
| `shprd settings show\|get\|set\|unset` | the settings: roles (`roles.worker.agent`, `roles.reviewer.model`, …) and workflow keys (`tasks.merge`, `tasks.max_running`, …), each printed with the layer it came from. `set` refuses a reviewer of the worker's model family |
| `shprd prompts list\|show\|path\|reset <id>` | the prompts each role starts with, stored under `~/.config/shprd/prompts/`; your edits are kept, `reset` brings back the shipped words |
| `shprd` / `shprd status` | tasks, agents, quota, and what needs you |
| `shprd brief` | a short, paste-able summary of the run |
| `shprd remote [--invite]` | remote access status; `--invite` shows a fresh pairing QR |
| `shprd gate` | run the repository's full checks (`[gates] integration`) once on the integration branch |
| `shprd sync` | merge the base branch into the integration branch when it moved under the run (a pull request merged meanwhile): a merge commit, never a rebase, so task commits keep their ids; a conflict is undone, escalated with the files, and exits 2. Landing needs a fresh `shprd gate` afterwards. The orchestrator runs it before `shprd gate`; no terminal needed |
| `shprd land` | push the integration branch and open or update the pull request; when the base has moved it says so and asks for `shprd sync`, then `shprd gate` |
| `shprd land --merge` | merge it: only with green checks, the reviewed head, and your approval |
| `shprd approve` | approve the open pull request at the integration branch's current head, for a solo maintainer who can't approve their own PR on GitHub; humans only, needs a terminal, and a new commit needs a new approval |
| `shprd readmit <agent> [--reason TEXT]` | clear a dry agent's mark so the run may spawn and hand off to it again (`shprd status` names the agent to pass); humans only, and unlike `approve` it needs no terminal |
| `shprd override --task <ID> --rule <RULE> --reason <TEXT>` | set a rule aside for one task. One rule honours it today: `integrate_after_failed_gate` lets the task integrate although a gate failed, a review included. Humans only, and it needs a terminal: the question names the task, the rule and the failed gate or review, and a 'no' or a missing terminal writes nothing. An orchestrator or an agent cannot record one, so the work a review stopped cannot be pushed through without you, and `shprd land` marks the pull request `degraded-review` when a failed review was overridden |
| `shprd watch` | the fallback loop: nudges stuck agents and hands off rate-limited ones. The service's tick records review verdicts, so `watch` is not needed for a run to finish |
| `shprd janitor [--apply]` | show (or apply) cleanup of what the run created: worktrees, branches, the merged remote branch |
| `shprd keys show\|install\|uninstall` | the `prefix+alt+…` key bindings in your herdr config |
| `shprd doctor` | check CLIs, sign-ins, skill visibility, mux, relay, git and gh; judges the roles and the cross-vendor pair from the settings |
| `shprd set orchestrator\|reviewer <agent> [--effort E]`, `shprd set workers <agent>…` | change the roster in your global config, or the repo's with `--repo <path>`; e.g. `shprd set reviewer codex/gpt-6-luna --effort high` |
| `shprd roster` | show agents, roles, models, availability |
| `shprd drop <task> --reason TEXT` | drop a task you decided not to do, so it stops blocking the run: it asks you at a terminal, naming the task, its status and its branch; a 'no' or a missing terminal writes nothing. Humans only. Nothing is deleted now — the worktree and branch stay, the janitor archives the branch before it removes either, and `shprd land`'s pull request lists the task and your reason under `## Dropped` |
| `shprd restore <task>` | bring back archived work |

Every command supports `--json`. Shell completion (`shprd completion zsh`) is planned, not built yet.

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
| `shprd agent task add` | orchestrator | define a task: title, scope, acceptance, tags, deps |
| `shprd agent spawn --task` | orchestrator | start a task (or its next attempt) on the next available worker, in its own worktree and tab |
| `shprd agent prompt --task` | orchestrator | send a message to a task's agent |
| `shprd agent report` | worker | submit status, tests, checks, changed paths, commit, skills used, and `--proof <path>` (never in the summary); one hand-in per attempt |
| `shprd task status <ID> --msg TEXT [--progress N]` | worker | record a short progress line |
| `shprd ask raise --kind question --text TEXT [--option O]…` | worker | raise a needs-you card yourself (programs with no ask hook, such as pi and opencode) and block until the person answers; prints the answer |
| `shprd ask hook` / `shprd ask install` | program hook / engine | the ask hook Claude Code and Codex call, and its install into the task worktree's own config (never your global config) |
| `shprd agent review --task [--degraded]` | orchestrator | open a reviewer of a different model family on the task's diff |
| `shprd agent review --task <ID> --again` | orchestrator | restart a stuck review (brief never arrived, reviewer hung): fresh reviewer tab, same attempt; refused once a verdict exists |
| `shprd agent review-verdict --task --attempt` | engine | record the verdict from the reviewer's own `findings.json`; code decides pass or fail. The service's tick runs it; anyone else is refused |
| `shprd agent integrate --task` | orchestrator | cherry-pick a passed task onto the integration branch |
| `shprd agent handoff --task` | engine (`watch`) | move a task to the next worker, keeping its work |
| `shprd agent escalate` | any | non-blocking note for a human, always with a stated default |

## The service

`shprd serve` binds 127.0.0.1 only. It writes `service.json` (port, id, pid,
version) and a bearer `token` to `$XDG_STATE_HOME/shprd/`; a client reads the port
there rather than guessing. On a clean stop it removes `service.json`.

| Route | Does |
|---|---|
| `GET /api/health` | `{ok, id, version}` |
| `GET /api/state` | the snapshot: tasks with their derived status (`Needs you`, `Working`, …), `needs`, `asks`, agents, `run` (null with a `hint` when there is no run yet) and `last_tick` (the last pass's steps and failures) |
| `GET /api/changes`, `GET /api/ws` | what changed since a seq / the same pushed over a socket |
| `GET /api/asks`, `POST /api/asks/answer` | open needs-you cards / answer one (`{task, ask, answer}`) |
| `POST /api/action` | the person's actions on a task |
| `GET\|PUT /api/settings`, `GET\|PUT /api/prompts/{id}` | the settings and prompts verbs over HTTP |

Every request needs the bearer token; every write also needs the header
`x-shprd: 1`. The tick (every `tasks.tick_secs`, at most `tasks.max_running`
tasks at once) starts queued tasks whose deps landed, starts the reviewer role on a
hand-in, records the verdict, sends a failed task back while review rounds are
left and escalates to a needs-you card when they are not, lands what
`tasks.merge` allows (asked about the branch the landing writes, not the base),
and closes the landed task's tabs. A failed step is logged on stderr at the level
`SHPRD_LOG` sets (`warn` by default) and kept in `last_tick`.

At startup `serve` creates or reuses the repo's workspace, labelled exactly
`shprd · <repo folder name>`.

## Config sections added in M5 and M6

```toml
[gates]
integration = ["cargo fmt --all --check", "cargo test --workspace"]  # run by `shprd gate`

[review]
must_fix = "major"   # findings this severe or worse fail the review
rounds = 2           # failed reviews before shprd escalates to you

[land]
merge = "ask"        # "ask": merge only after your approval (GitHub or `shprd approve`); "allow": also without it, never into a protected branch
protected = ["main", "master"]
```

Reviewers never run with auto-approve. A harness spec can confine a reviewer's
writes with `reviewer_args` (Codex: `--sandbox workspace-write`, so it can only
write inside its review folder); other CLIs ask you before their reviewer uses a
tool.
