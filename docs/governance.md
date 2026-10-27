# Self-governance

Status: design.

Goal: agents never need to ask "may I delete this?", and you never clean up
after a run by hand. The approach is to narrow what shprd may touch and then let
it act freely inside that.

## Rules

1. **Ownership ledger.** Every resource the engine creates is an event, tagged
   with run and task: local branch, remote branch, worktree, PR, tab, stash,
   temp dir. Cleanup only touches resources in the ledger.
2. **Namespaces.** Branches `shprd/<run>/<task>`, integration branch
   `shprd/<run>/int`, worktrees under `~/.shprd/wt/<repo>/<run>/<task>`. The
   engine's verbs refuse destructive git operations outside the namespace.
3. **Archive before delete.** Before removing a branch or worktree, the engine
   writes `refs/shprd/archive/<run>/<task>`, plus a bundle for unpushed work.
   These are kept for 14 days by default. `shprd restore <task>` brings work
   back.
4. **Tiers decided by code:**

   | Tier | Examples | Behaviour |
   |---|---|---|
   | auto | delete merged `shprd/*` branches (local and remote), remove landed or abandoned worktrees, close tabs, prune stale refs | janitor, no prompt, logged |
   | auto + notify | force-push within a task's own branch, re-create a conflicted branch, close a PR this run opened | logged, one quiet line in the chat |
   | ask | anything on a non-`shprd/*` branch, protected branches, rewriting shared history, deleting unpushed work with no archive | waits for you, in the chat and as a phone card |

5. **The janitor runs on events.** It runs when a PR merges or closes, when a
   task is abandoned, at `shprd up`, and nightly. It's code, not an agent.

## Credentials

Only the engine pushes. Worker panes start with an environment that strips push
credentials:

- `GH_TOKEN` and `GITHUB_TOKEN` unset;
- `GIT_TERMINAL_PROMPT=0`;
- `credential.helper` blanked through `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_*` /
  `GIT_CONFIG_VALUE_*`.

This is a soft boundary: a determined agent could still find your credentials.
It guarantees that routine paths can't push, not that agents are sandboxed. On
tuios, workers also run without the `admin` pane grant.

## Optional server-side backstop

A GitHub ruleset can protect `main` and restrict who may create, push or delete
branches outside `shprd/*`. It only distinguishes agents from you if shprd
pushes as a separate identity (a GitHub App, a bot account or a fine-grained
token). On private repos, rulesets may need a paid plan.
