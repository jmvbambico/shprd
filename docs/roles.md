# Roles and tabs

Status: design.

## Roles

| Symbol | Role | Does | Never |
|---|---|---|---|
| ★ | Orchestrator | talks to you, plans, splits work into tasks, adds skills to tasks | edits code itself; writes status files |
| ● | Worker | implements one task in its own worktree, runs worker-level checks, reports | creates its own worktrees, merges, pushes, spawns sub-agents |
| ◇ | Reviewer | reviews a diff against acceptance criteria and lists findings with severity | edits files; sees a worktree |
| ◌ | (state) | an agent that is dry (out of quota) or waiting | |

Integration, landing and cleanup are **not** roles. The engine does them
deterministically.

The reviewer is always a different vendor than the implementer. If that's
impossible, the run says so and the PR is labelled `degraded-review`.

Exactly one CLI is the orchestrator, chosen in `shprd setup` by starring it.
Workers are an ordered list, and the order is the fallback order.

## Tabs

One workspace per repo, named `shprd · <repo>`.

| Position | Name | Registered as an agent? |
|---|---|---|
| 1 | `shprd·chat` | no |
| 2 | `★ <cli>·orchestrator` | yes |
| 3… | `● <cli>·<task-slug>` | yes |
| as needed | `◇ <cli>·<task-slug>` | yes, open only during a review round |

- The task slug is the task's short name, cut to 16 characters. There is no
  `T<n>` prefix.
- When two live tabs would share a name, the worktree hash is appended:
  `● pi·middleware·a41c`.
- The symbol changes in place with state: `●` becomes `◌` when an agent goes dry.
- A hand-off renames the tab to the new CLI (`● opencode·middleware` →
  `● pi·middleware`). [dagr](architecture.md) keeps the history.
- When a task lands, the janitor closes its tab.

On tuios, which has no tabs, the same names go on windows. See
[Multiplexers](multiplexers.md).
