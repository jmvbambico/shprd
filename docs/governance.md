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
3. **Archive before delete.** Before deleting a branch, the engine writes an
   archive ref named after that branch (`shprd/<run>/<task>` →
   `refs/shprd/archive/<run>/<task>`, so a retry branch gets its own ref). A
   branch is deleted only if the ledger recorded creating it; removing a
   worktree never deletes a branch. Archive refs are kept for
   `janitor.archive_days` (14 by default), counted from the creation that is
   current in the ledger. `shprd restore <task>` brings the task's branch back
   at the archived commit, and checks the ledger before creating anything.
   (Bundles for unpushed work exist but aren't used by the janitor yet.)
4. **Tiers decided by code:**

   | Tier | Examples | Behaviour |
   |---|---|---|
   | auto | delete merged `shprd/*` branches (local and remote), remove landed or abandoned worktrees, close tabs, prune stale refs | janitor, no prompt, logged |
   | auto + notify | force-push within a task's own branch, re-create a conflicted branch, close a PR this run opened | logged, one quiet line in the chat |
   | ask | anything on a non-`shprd/*` branch, protected branches, rewriting shared history, deleting unpushed work with no archive | waits for you, in the chat and as a phone card |

5. **The janitor runs on events.** It runs when a PR merges or closes, when a
   task is abandoned, at `shprd up`, and nightly. It's code, not an agent.

## Autonomy styles (chosen in `shprd setup`)

Setup has a **Security** step that asks how much you want to approve by hand.
Each style sets a few switches; you can override any of them later with
`shprd set`.

| Style | Orchestrator | Workers | Reviewer | Merge |
|---|---|---|---|---|
| **Hands-on** | asks before each command | ask before each tool use | asks (Codex: sandboxed) | you approve the PR |
| **Supervised** | asks before each command | auto-approve | Codex sandboxed, others ask | you approve the PR |
| **Autonomous to PR** *(recommended)* | auto-approve | auto-approve | Codex sandboxed, others ask | you approve the PR |
| **Fully autonomous** | auto-approve | auto-approve | Codex sandboxed, others ask | merges itself when checks pass, **never into a protected branch** |

What each style really guarantees, shown on the screen next to the choice:

- **Hands-on / Supervised:** the orchestrator can't run anything you didn't
  approve, so the PR gate is enforced by you at every step.
- **Autonomous to PR:** the orchestrator runs unprompted. The PR gate is a rule
  in its brief (never `git push`, `gh pr merge` or `gh pr review`; land only
  through `shprd land`, merge only after you approve). Nothing outside shprd
  enforces it unless `main` requires an approving review from someone other than
  the PR's author.
- **Fully autonomous:** as above, plus `[land] merge = "allow"`; protected
  branches still need your approval.

Switches behind the styles: `roster.orchestrator_auto_approve`,
`roster.worker_auto_approve` (default on), and `land.merge`. Reviewers never
auto-approve in any style. `shprd doctor` prints a `!` line whenever the
orchestrator runs with auto-approve.

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
