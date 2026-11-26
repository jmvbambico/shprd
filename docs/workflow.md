# Run lifecycle

Status: design.

```
shprd up ─► plan ─► spawn ─► work ─► report ─► review ─► integrate ─► full gate ─► PR ─► you approve ─► land ─► janitor
                      ▲                          │
                      └──── sent back ◄──────────┘ (blocking finding)
```

1. **Up.** `shprd up` checks preconditions: a repo, a branch rather than a
   detached HEAD, a clean tree, the CLIs signed in. It then opens or reuses the
   repo's workspace, starts `shprd·chat`, and starts the orchestrator CLI in its
   own tab. An unfinished run is resumed, never restarted.
2. **Plan.** You tell the orchestrator what you want. It splits the work into
   tasks, each with declared paths (where it expects to work, a hint), acceptance criteria, and tags
   (`ui`, `bug`, `feature`, `docs`, …), using `shprd agent task add`.
3. **Spawn.** For each task, the engine picks the first *available* worker in
   the roster, creates a worktree on `shprd/<run>/<task>`, opens a tab, and
   hands over a brief that names the skills to load. Declared paths are a hint,
   not a limit: a worker changes what the task needs. Tasks that depend on each
   other are ordered with `--dep`; two tasks that clash are caught by the
   pre-review trial merge and again by the landing merge.
4. **Work.** The worker implements the task and runs the repo's worker-level
   checks. The watchdog follows it (see [Availability](availability.md)).
5. **Report.** The worker submits `shprd agent report` with its tests, changed
   paths, commit and the skills it used. The engine checks the report against what it saw run. No hand-in, review
   or landing is refused for the paths it changed. The only hard refusals are
   committing `.shprd/` (except `.shprd/config.toml`), `.dagr/` or a real
   `.env*` file. A task that touches a protected path it did not declare waits
   for your Accept, even under auto-merge.
6. **Review.** A reviewer from a different vendor gets the diff and the
   acceptance criteria as text. It never gets a worktree. Code decides whether
   the work passes, from fixed severity rules. A blocking finding sends the task
   back as a new attempt.
7. **Integrate.** The engine cherry-picks the verified commits onto
   `shprd/<run>/int`.
8. **Full gate.** The repo's full checks run once, on the integration branch.
   This is the only place the integration suite runs.
9. **PR.** The engine pushes the integration branch and opens one PR. The PR
   body is generated from the event log: every review finding, every override,
   and every hand-off.
10. **You approve.** In the terminal, in `shprd·chat`, or from your phone.
11. **Land.** The engine merges according to your merge policy.
12. **Janitor.** Branches, worktrees and tabs the run created are archived and
    removed. See [Self-governance](governance.md).

Human moments: kickoff, an occasional re-steer, and approval. Anything else that
needs you appears pinned above the chat composer and as a phone card.
