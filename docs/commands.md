# Commands

Status: built through M6 (`setup` and `remote` are M7).

## For you

| Command | Does |
|---|---|
| `shprd setup` | the setup wizard (TUI): agents, models, skills, remote, layout |
| `shprd up` | start or resume this repo's run |
| `shprd down` | finish the run; refuses (exit 2, nothing written) while any task is unfinished or the run's pull request has not merged, and names each task and how to proceed: `shprd up` restarts the orchestrator or watchdog and resumes this run. `shprd down --abandon` (you only, never an agent or the orchestrator) asks you to confirm at a terminal, naming the run and each task it would leave with its branch; a 'no' or a missing terminal writes nothing and closes nothing. On a 'yes' it ends the run anyway, records which tasks were left (with an unmerged pull request, the integrated tasks it carried too), and keeps their branches. The orchestrator may end a run normally once every task is integrated and the pull request merged |
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
| `shprd watch` | the watchdog loop: records review verdicts and hands off stuck or rate-limited agents. `shprd up` runs it in a pane of tab 1, so you rarely start it yourself |
| `shprd janitor [--apply]` | show (or apply) cleanup of what the run created: worktrees, branches, the merged remote branch |
| `shprd keys show\|install\|uninstall` | the `prefix+alt+…` key bindings in your herdr config |
| `shprd doctor` | check CLIs, sign-ins, skill visibility, mux, relay, git and gh |
| `shprd set orchestrator\|reviewer <agent> [--effort E]`, `shprd set workers <agent>…` | change the roster in your global config, or the repo's with `--repo <path>`; e.g. `shprd set reviewer codex/gpt-6-luna --effort high` |
| `shprd roster` | show agents, roles, models, availability |
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
| `shprd agent report` | worker | submit status, tests, changed paths, commit, skills used |
| `shprd agent review --task [--degraded]` | orchestrator | open a reviewer of a different model family on the task's diff |
| `shprd agent review --task <ID> --again` | orchestrator | restart a stuck review (brief never arrived, reviewer hung): fresh reviewer tab, same attempt; refused once a verdict exists |
| `shprd agent review-verdict --task --attempt` | engine | record the verdict from the reviewer's own `findings.json`; code decides pass or fail. The watchdog runs it; anyone else is refused |
| `shprd agent integrate --task` | orchestrator | cherry-pick a passed task onto the integration branch |
| `shprd agent handoff --task` | engine (`watch`) | move a task to the next worker, keeping its work |
| `shprd agent escalate` | any | non-blocking note for a human, always with a stated default |

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
