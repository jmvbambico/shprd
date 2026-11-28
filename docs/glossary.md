# Glossary

| Term | Meaning |
|---|---|
| **run** | one shprd session on one repo, from `shprd up` to land; has an id like `7f3a` |
| **task** | one unit of work with declared paths, acceptance criteria and tags; runs in its own worktree |
| **declared paths** | where a task expects to work: a hint for its worker, never a limit. A task may change what it needs |
| **protected path** | a path on AGENTS.md's "Don't touch" list (kept in `.agents/orchestration.yaml` `high_risk_paths`, plus built-ins). A task touching one it did not declare waits for your Accept, even under auto-merge |
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
| **space** | one per repo, labelled `shprd · <repo>`; holds the coordinator. A task gets no space of its own |
| **coordinator** | the CLI the service starts in the space; shown as the space's Assistant chat |
| **live agent** | a worker, reviewer or coordinator the host lists (`live_agents`); finished ones stay in the Agents pane for 30 minutes |
| **thread** | a task's view, switching between Chat and Terminal |
| **session** | a browser's login: a cookie naming a stored hash, ended by idle, age or `shprd web sessions --revoke` |
| **gate (remote)** | the checks in front of every API route but health and login: `Host`, credential, `Origin`, `x-shprd`, rate limits |
| **tunnel** | a provider's way of putting the loopback port at an address: Tailscale serve, cloudflared, ngrok, tunnl.gg |
| **dagr** | the task-graph viewer; shprd writes its run file |
