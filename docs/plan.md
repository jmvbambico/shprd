# Build plan

Status: proposed 2026-10-27, awaiting approval. Language: Rust.

The plan is cut into milestones. Each one ends at a **gate**: a demo you can run
and a check that has to pass before the next milestone starts. Inside a
milestone, tasks are split by directory so several agents can work in parallel
without touching the same files (see "Parallel lanes").

```
M0 foundations ─► M1 engine ─┬─► M2 git ─────────┐
                             ├─► M3 roster ──────┼─► M5 watchdog ─┐
                             └─► M4 herdr + up ──┘                ├─► M6 review & land ─► ★ dogfood ─► M7 TUI ─► M8 tuios ─► M9 v0.1.0
                                                                  │
                                       (M3 also feeds M6) ────────┘
```

★ **Dogfood point:** after M6, shprd can run its own development from the
command line, without the TUI. From then on we build shprd with shprd and leave
og.

---

## Workspace layout (decided in M0, then fixed)

```
crates/
  shprd-core/       events, fold, task model, transition rules, config      (no I/O beyond the log file)
  shprd-views/      projectors: dagr run.json, brief, status, PR body       (pure functions of events)
  shprd-git/        worktrees, scopes, integration branch, archive refs, janitor
  shprd-roster/     harness table, CLI detection, sign-in, models, quota probes, skills discovery
  shprd-watch/      watchdog signals and the dry/hand-off state machine
  shprd-mux/        host adapter trait + capability flags
  shprd-mux-herdr/  herdr adapter
  shprd-mux-tuios/  tuios adapter
  shprd-tui/        setup wizard, shprd·chat, remote box (ratatui)
  shprd/            the binary: clap CLI, wiring
data/
  harness/*.toml    one file per agent CLI
  skills/compat.toml
```

Crate boundaries are the lane boundaries. Two agents never work in the same
crate in the same milestone unless the task says so.

## Gates (proposed for AGENTS.md)

| Gate | Command | Who runs it |
|---|---|---|
| format | `cargo fmt --all --check` | workers, CI |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | workers, CI |
| unit tests | `cargo test --workspace` | workers, CI |
| integration tests | `cargo test --workspace --features it -- --test-threads=1` | integration branch only |

Integration tests create real git repos in temp dirs and use a **fake mux**, so
they need no herdr, tuios or model calls. Tests that need a real CLI or mux are
marked `#[ignore]` and run by hand at milestone gates.

---

## M0 · Foundations

Goal: a repo that agents can work in safely.

| ID | Task | Scope | Size |
|---|---|---|---|
| M0.1 | `AGENTS.md`: gates, branch policy, review rule, never-write paths, commit style | `AGENTS.md` | S (docs) |
| M0.2 | Cargo workspace with the crates above as empty libs, the `shprd` binary printing `--version`, shared lints in `Cargo.toml` | `Cargo.toml`, `crates/*/` | S |
| M0.3 | CI: GitHub Actions running format, lint and unit tests on push and PR, macOS + Linux | `.github/workflows/` | S |
| M0.4 | `data/harness/` schema (a `HarnessSpec` serde type in `shprd-roster`) plus the five CLI files, filled from what we verified in [Agents](agents.md) | `data/harness/`, `crates/shprd-roster/src/spec.rs` | M |

Order: M0.1 → M0.2 → (M0.3 ∥ M0.4).

**Gate:** CI green on `main`. `cargo run -- --version` works. `AGENTS.md`
approved by you.

---

## M1 · Engine core (no git, no mux)

Goal: the event log is the truth, and every view is rendered from it.

| ID | Task | Scope | Size |
|---|---|---|---|
| M1.1 | Event types (`TaskAdded`, `Spawned`, `Reported`, `GatePassed`, `GateFailed`, `Integrated`, `Dry`, `HandedOff`, `Escalated`, `Override`, …), append-only JSONL log with fsync and a file lock, replay | `shprd-core/src/{event,log}.rs` | M |
| M1.2 | Fold to `RunState`; transition rules returning typed errors (e.g. integrating with an open failed gate is refused unless there's an `Override`) | `shprd-core/src/{state,rules}.rs` | M |
| M1.3 | Config: global + repo TOML, merged field by field; roster, skills, workspace, janitor sections | `shprd-core/src/config.rs` | S |
| M1.4 | dagr projector: events → dagr v3 document; tmp → `dagr check --strict` (if dagr is on PATH) → rename; golden-file tests | `shprd-views/src/dagr.rs` | M |
| M1.5 | `brief`, `status` and PR-body projectors, golden-file tests | `shprd-views/src/{brief,status,pr}.rs` | S |
| M1.6 | CLI: `shprd agent task add / report / gate / escalate`, `shprd brief`, `shprd status`, all with `--json` | `crates/shprd/src/cmd/` | M |

Lanes: (M1.1 → M1.2) ∥ M1.3; then M1.4 ∥ M1.5 ∥ M1.6.

**Gate:** a scripted fixture run (add 3 tasks, report, fail a gate, override,
pass) produces a `run.json` that passes `dagr check --strict`, plus stable
`brief` and PR-body output. Replaying the log after deleting all derived files
reproduces them byte for byte.

---

## M2 · Git layer

Goal: worktrees, scopes, integration and cleanup, all owned by the engine.

| ID | Task | Scope | Size |
|---|---|---|---|
| M2.1 | Git runner (no shell), ownership ledger event in core (`Resource` created/archived/removed), worktree create/remove under `~/.shprd/wt/<repo>/<run>/<task>` on branch `shprd/<run>/<task>`, and changed-paths-of-a-range checked against the task scope (the scope rules already live in `shprd-core::scope` from M1) | `shprd-git/src/{git,worktree,changed}.rs`, `shprd-core` events/state/rules | M |
| M2.3 | Integration: verify claimed commits, cherry-pick onto `shprd/<run>/int`, conflict → event, clean-tree guards | `shprd-git/src/integrate.rs` | M |
| M2.4 | Archive refs + bundles, `shprd restore`, local janitor (ledger-owned only, namespace-guarded) | `shprd-git/src/{archive,janitor}.rs` | M |

M2.2 was folded into M2.1: M1 already built the scope rules in `shprd-core::scope`.

Lanes: M2.1 first (the runner and ledger everything else uses), then M2.3 ∥ M2.4.

**Gate:** integration tests in temp repos: overlapping scopes refused, a commit
outside scope refused, conflict recorded and work retained, janitor never
touches a non-`shprd/*` branch, restore round-trips.

---

## M3 · Roster

Goal: shprd knows which agents exist, can run, and have quota.

| ID | Task | Scope | Size |
|---|---|---|---|
| M3.0 | Harden `spec.rs` (from the M0 review): reject empty locations and misplaced `*`; reject an unmatched `{` in templates; validate launch templates at load (placeholder fits its field, `model_args` uses `{model}`); `load_dir` runs `validate` | `shprd-roster/src/spec.rs` | S |
| M3.1 | Detection: binary, version, sign-in probes per harness file; `shprd roster` | `shprd-roster/src/detect.rs` | M |
| M3.2 | Models: list per CLI where possible; `program/model` + effort parsing; `shprd set` | `shprd-roster/src/models.rs`, `shprd/src/cmd/set.rs` | S |
| M3.3 | Quota probes: Claude OAuth usage and Codex wham usage ported from og (`installer/og_quota.py:168-318`, credited); kilo profile; cache and 429 fallback | `shprd-roster/src/quota/` | M |
| M3.4 | Skills: discover global + native per CLI **in the launch environment**, compat list, folder-hash change detection | `shprd-roster/src/skills.rs`, `data/skills/compat.toml` | M |
| M3.5 | Clean launch environment builder (strip push credentials, drop og-style overrides) | `shprd-roster/src/env.rs` | S |
| M3.6 | `shprd doctor` | `shprd/src/cmd/doctor.rs` | S |

Lanes: M3.0 first (same file as later roster work); then M3.1 ∥ M3.3 ∥ M3.4 ∥ M3.5; then M3.2 ∥ M3.6.

**Gate:** `shprd roster` and `shprd doctor` on this machine correctly report the
five CLIs, their sign-in state, Claude and Codex quota, and which skills each CLI
sees.

---

## M4 · herdr adapter and `shprd up`

Goal: shprd can lay out a repo workspace and drive agents in herdr.

| ID | Task | Scope | Size |
|---|---|---|---|
| M4.1 | `Mux` trait with capability flags, plus a **fake mux** for tests | `shprd-mux/` | S |
| M4.2 | herdr adapter: workspace per repo, tabs, naming and renaming (★ ● ◇ ◌), split with ratio, run command, send prompt, read screen, agent-state events | `shprd-mux-herdr/` | L |
| M4.3 | `shprd up` / `shprd down`: preconditions, workspace reuse, orchestrator tab, resume | `shprd/src/cmd/{up,down}.rs` | M |
| M4.4 | `shprd agent spawn` / `shprd agent prompt`: worktree + tab + brief with skills | `shprd/src/cmd/agent/` | M |
| M4.5 | Keybind block writer for herdr (marked block; refuses to touch anything else) | `shprd-mux-herdr/src/keys.rs` | S |

Lanes: M4.1 first; then M4.2 (long, give it to a long-running worker) ∥ M4.5;
then M4.3 ∥ M4.4.

**Gate (manual, on herdr):** `shprd up` in a scratch repo opens
`shprd · <repo>` with `shprd·chat` (a placeholder for now) and
`★ <cli>·orchestrator`. `shprd agent spawn` opens `● <cli>·<task>` in its own
worktree. Running `shprd up` twice doesn't duplicate tabs.

---

## M5 · Watchdog and hand-off

Goal: no more silent stalls.

| ID | Task | Scope | Size |
|---|---|---|---|
| M5.1 | Record readers for claude, codex, opencode/kilo (SQLite), pi: provider errors + reset times | `shprd-watch/src/records/` | M |
| M5.2 | opencode/kilo `serve` status client: `GET /session/status`, `/event` SSE `session.status` retry | `shprd-watch/src/serve.rs` | M |
| M5.3 | Screen signatures + liveness sampling via the mux | `shprd-watch/src/screen.rs` | S |
| M5.4 | State machine working → suspect → dry → available; retry schedule; canary re-admit | `shprd-watch/src/machine.rs` | M |
| M5.5 | Hand-off: checkpoint commit, pick next worker, generated brief, same worktree, tab rename | `shprd-watch/src/handoff.rs` | M |

Lanes: M5.1 ∥ M5.2 ∥ M5.3; then M5.4; then M5.5.

**Gate:** with the fake mux, a scripted "agent goes silent" is marked dry within
5 minutes and handed off within 60 seconds of being marked dry. Manual check: a
real worker killed mid-task is detected and its task continues on the next
worker.

---

## M6 · Review and landing

Goal: work reaches `main` through a reviewed PR, and the run cleans up.

| ID | Task | Scope | Size |
|---|---|---|---|
| M6.1 | Reviewer dispatch: diff + acceptance as text, cross-vendor check, findings schema | `shprd/src/cmd/agent/review.rs`, `shprd-core/src/review.rs` | M |
| M6.2 | Gate rules: severities, `mustFix`, rounds, send-back as a new attempt | `shprd-core/src/gate.rs` | S |
| M6.3 | Full gate runner on the integration branch (repo-defined commands from `AGENTS.md` / config) | `shprd-git/src/gate.rs` | M |
| M6.4 | Landing: push, `gh pr create` with the generated body, merge policy (allow / ask / deny), `shprd land` | `shprd-git/src/land.rs`, `shprd/src/cmd/land.rs` | M |
| M6.5 | Remote janitor: delete merged `shprd/*` branches, close tabs, prune | `shprd-git/src/janitor.rs` | S |

Lanes: M6.1 ∥ M6.2 ∥ M6.3; then M6.4; then M6.5.

**Gate (★ dogfood):** shprd implements one real shprd task end to end on herdr:
plan, spawn, review, integrate, full gate, PR, your approval, land, cleanup.
From this point, shprd is built with shprd.

---

## M7 · TUI

| ID | Task | Scope | Size |
|---|---|---|---|
| M7.1 | Transcript readers for the orchestrator CLIs (claude and codex first) | `shprd-tui/src/transcript/` | M |
| M7.2 | `shprd·chat`, Transcript design: messages, event cards, pinned attention, composer, `/` commands | `shprd-tui/src/chat/` | L |
| M7.3 | Right column: `dagr view` split + remote box (collapsed, expanded, invite with countdown via the relay's `setup-link`) | `shprd-tui/src/{layout,remote}.rs` | M |
| M7.4 | `shprd setup` wizard: agents → skills → remote & layout → write | `shprd-tui/src/setup/` | L |

Lanes: M7.1 ∥ M7.3 ∥ M7.4; then M7.2.

**Gate:** the prototype screens (`research/prototypes/shprd-tui.html`) are
reproduced in a real terminal, and a full run is driven from `shprd·chat` and
from the phone via the relay.

---

## M8 · tuios adapter

| ID | Task | Scope | Size |
|---|---|---|---|
| M8.0 | Run the herdr adapter against tuios's herdr-compatible socket (`herdr_protocol = "always"`); list what breaks. Also test herdr-mobile-relay on tuios | report only | S |
| M8.1 | tuios protocol client (JSON lines on the daemon socket, `hello` feature flags) | `shprd-mux-tuios/src/proto.rs` | M |
| M8.2 | Adapter: session per repo, workspaces 1–4, windows and naming, events via `subscribe`, pane grants | `shprd-mux-tuios/src/adapter.rs` | L |
| M8.3 | Keybind writer for `[[keybindings.command]]`; remote box shows `tuios-web` | `shprd-mux-tuios/src/keys.rs`, `shprd-tui/src/remote.rs` | S |

**Gate:** the M6 dogfood scenario passes on tuios.

---

## M9 · v0.1.0

| ID | Task | Size |
|---|---|---|
| M9.1 | Skills curation pack published as an `npx skills` source (`shprd-orchestrator` + compat list) | M |
| M9.2 | Install paths: `cargo install`, Homebrew tap, release binaries via CI | M |
| M9.3 | herdr thin plugin (actions → `shprd <verb>`), marketplace listing | S |
| M9.4 | `THIRD_PARTY_NOTICES.md` audit, README status table updated, release notes | S |
| M9.5 | Release on every merge (below) | M |

### Releases after v0.1.0

From v0.1.0 on, **every PR merged into `main` produces a tagged, published
release.** No release PRs and no manual tagging.

- **Trigger:** a GitHub Actions workflow on `push` to `main`. Merging an
  approved PR is the only way anything reaches `main`.
- **Next version** comes from the Conventional Commits in the merge since the
  last tag:

  | Commits contain | Before 1.0 | From 1.0 |
  |---|---|---|
  | `feat!` or `BREAKING CHANGE` | minor | major |
  | `feat` | minor | minor |
  | `fix`, `perf` | patch | patch |
  | only `docs`, `chore`, `ci`, `test`, `refactor` | patch | patch |

  Every merge releases, so a docs-only PR still ships a patch. Its notes say
  "no functional change".
- **Tag, don't commit.** The workflow creates and pushes tag `vX.Y.Z` and does
  **not** push a version-bump commit. That way protected `main` never needs a
  bot that bypasses the PR rule. The build stamps the version from the tag
  (`SHPRD_VERSION` env read in `build.rs`, falling back to `Cargo.toml`), so
  `shprd --version` matches the release. `Cargo.toml` stays at the last minor
  version and is bumped by hand only when publishing to crates.io.
- **Publish:** build release binaries (macOS arm64 + x86_64, Linux x86_64 +
  arm64), attach them with checksums to a GitHub Release, and write notes
  grouped by commit type with each merged PR linked. Then update the Homebrew
  tap formula.
- **Safety:** the workflow refuses to tag if the CI run for that commit didn't
  pass, or if the tag already exists. A failed publish leaves no tag behind.
- **Before v0.1.0:** no automatic releases. Milestone merges stay untagged
  until M9 ships v0.1.0.

---

## How each milestone runs

1. **Kickoff.** I post the milestone's task list with scopes and acceptance
   criteria. You approve or adjust it.
2. **Fan-out.** Tasks in the same lane step go out in parallel, one worktree
   each. Small bounded tasks go to the free-tier workers; tasks marked L or
   long-running go to Pi.
3. **Integrate.** Finished tasks are combined on one integration branch, and
   the integration gate runs there once.
4. **Review.** The combined diff goes to the reviewer (Codex, a different vendor
   from the DeepSeek and Kilo workers) with the acceptance criteria.
5. **PR.** One PR per milestone step, with review findings in the body. You
   approve the merge.
6. **Milestone gate.** The demo above is run and recorded in
   [Decisions](decisions.md) before the next milestone starts.

## Open decisions before M0 starts

1. **Branch policy:** I propose `main` protected, with every change landing by
   PR. Integration branches are `integration/m<N>` and task branches `feature/<task>`. You merge.
2. **CI platforms:** macOS + Linux. Windows (WSL only) deferred.
3. **Minimum Rust version:** stable at M0 time, pinned in `rust-toolchain.toml`.
4. **TUI library:** ratatui + crossterm (proposed).
