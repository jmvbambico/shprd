# Skills

Status: design. Curation is a maintainer commitment.

## Two sources

| Source | Where | How shprd treats it |
|---|---|---|
| **Global** | `~/.agents/skills`, managed with `npx skills` | routed by role and task tag; filtered by the curated compatibility list |
| **CLI-native** | each CLI's own folder (`~/.claude/skills`, `~/.codex/skills`, `~/.config/opencode/skill(s)`, `~/.pi/agent/skills`, …) | left as-is, never hidden; flagged in setup if it teaches a conflicting workflow |

What each CLI reads, as checked on 2026-10-27:

| CLI | Reads |
|---|---|
| claude | `~/.claude/skills` |
| codex | `~/.codex/skills`, `.agents/skills` (live check pending) |
| opencode | `~/.config/opencode/skill(s)`, `~/.agents/skills`, `~/.claude/skills` |
| kilo | `~/.agents/skills`, `~/.claude/skills` |
| pi | `~/.pi/agent/skills`, `~/.agents/skills` |

`~/.agents/skills` is the shared home. Claude reaches it through symlinks in
`~/.claude/skills`; setup offers to add missing links. Discovery also picks up
junk such as `.trash/` and `synced/` folders, so setup lists duplicates and stale
entries.

## Routing

Each task carries tags. A rules table maps tags to skills, so the orchestrator
doesn't have to remember them:

| Tag | Skills |
|---|---|
| all worker tasks | verification-before-completion, receiving-code-review |
| `bug` | systematic-debugging, test-driven-development |
| `feature` | test-driven-development |
| `ui` | design-taste-frontend, make-interfaces-feel-better (+ opt-in frontend-design, ui-ux-pro-max, agent-browser, better-icons) |
| `perf` | web-perf |

The orchestrator may **add** skills to a task. Removing a required one is logged
as an override.

| Role | Default skills |
|---|---|
| Orchestrator | brainstorming (kickoff only, with you present), writing-plans, dispatching-parallel-agents, requesting-code-review, receiving-code-review, shprd-orchestrator |
| Reviewer | receiving-code-review, simplify; web-design-guidelines for `ui` |

## Enforcement

A prompt can't force a model to use a skill. shprd enforces it where code can:

1. **Route:** skills come from tags, by rule.
2. **Instruct:** the worker brief opens with the exact skills to load.
3. **Check:** the report must list `skills_used` with evidence, and the engine
   and reviewer check it. Some skills leave checkable traces:
   test-driven-development (test commit first), verification-before-completion
   (reported tests match what the engine saw run).

## Off by default (incompatible with the shprd flow)

| Skill | Why |
|---|---|
| subagent-driven-development | spawns hidden sub-agents outside shprd's scopes and fallback |
| executing-plans | assumes one agent runs the whole plan inline |
| using-git-worktrees | shprd owns worktrees |
| finishing-a-development-branch | shprd owns merge, PR and cleanup |
| planning-with-files | writes plan files into the repo; the event log does this |
| using-superpowers | a skill check before every reply; tag rules do it for free |
| find-skills | installs skills mid-run |

## Curation

The maintainers keep a compatibility list (allow / deny / reason) for popular
skills. It will be published as an `npx skills` source with the
`shprd-orchestrator` skill. Each verdict records the skill's folder hash from
`~/.agents/.skill-lock.json`. When an upstream skill changes, `shprd doctor`
flags it for re-review.
