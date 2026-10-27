# Glossary

| Term | Meaning |
|---|---|
| **run** | one shprd session on one repo, from `shprd up` to land; has an id like `7f3a` |
| **task** | one unit of work with a scope, acceptance criteria and tags; runs in its own worktree |
| **scope** | the paths a task may change; overlapping scopes don't run in parallel |
| **orchestrator** (★) | the one CLI that talks to you and plans |
| **worker** (●) | a CLI implementing one task |
| **reviewer** (◇) | a CLI reviewing a diff; always a different vendor than the implementer |
| **dry** (◌) | an agent that is out of quota or not responding |
| **hand-off** | moving a task from a dry agent to the next, from a checkpoint commit |
| **engine** | shprd's own code that owns state, validates verbs, runs gates, lands and cleans up |
| **event log** | `.shprd/events.jsonl`, the single source of truth for a run |
| **projector** | code that renders a view (dagr, brief, PR body) from the event log |
| **gate** | a check that decides whether work moves on: worker checks, review, full gate |
| **full gate** | the repo's complete checks, run once on the integration branch |
| **integration branch** | `shprd/<run>/int`, where verified task commits are cherry-picked |
| **land** | merge the approved PR and clean up |
| **janitor** | engine code that archives and removes what a run created |
| **harness table** | per-CLI data: how to detect, launch, read records and spot quota errors |
| **host adapter** | the thin layer that talks to herdr or tuios |
| **mux** | terminal multiplexer: herdr or tuios |
| **relay** | herdr-mobile-relay, phone and browser access on herdr |
| **dagr** | the task-graph viewer; shprd writes its run file |
