# AGENTS.md

The rules for every agent working in this repo, human or AI. This file wins
over any other instruction file, skill or prompt that disagrees with it.

shprd is a Rust workspace that orchestrates coding agents inside the herdr and
tuios terminal multiplexers. Read [docs/README.md](docs/README.md) for the
design. Read [docs/plan.md](docs/plan.md) for what's being built now.

## Gates

| Gate | Command | Run by workers | Run on the integration branch | Run by CI |
|---|---|---|---|---|
| format | `cargo fmt --all --check` | yes | yes | yes |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | yes | yes | yes |
| unit tests | `cargo test --workspace` | yes | yes | yes |
| integration tests | `cargo test --workspace --features it -- --test-threads=1` | **no** | yes | no |

- Workers run the first three gates in their own worktree before reporting
  done, and say which ones they ran.
- The integration tests run only on the integration branch, once per batch,
  and never in two places at once.
- Until a crate defines the `it` feature, the integration-tests row is run as
  `cargo test --workspace -- --test-threads=1`.
- Tests that need a real CLI, a real multiplexer or a model call are
  `#[ignore]` and are run by hand at milestone gates. CI never makes model
  calls.

## Branches and merging

| Branch | Purpose | Who writes it |
|---|---|---|
| `main` | released history; protected, changes only by pull request | merged by the maintainer (a human) |
| `integration/m<N>` | one milestone's integration branch, cut from `main` | the orchestrator lands task branches here |
| `feature/<task-slug>` | one task, one worktree | one worker |

- **Integration base:** `integration/m<N>` for the current milestone.
- **Never** push to `main`, open or merge a PR, force-push a shared branch, or
  delete a branch you didn't create. Workers commit on their own
  `feature/` branch only. The orchestrator opens the milestone PR, and the
  maintainer merges it.
- Worktrees live outside the repo at `~/projects/.worktrees/shprd/<task-slug>`.
  Never edit the main checkout from a worker.

## Review

- Every change is reviewed by an agent from a **different vendor** than the
  one that wrote it. The reviewer gets the diff and the task's acceptance
  contract as text, never a worktree, and never edits.
- If cross-vendor review is impossible, say so and label the PR
  `degraded-review`.
- The PR body records the outcome: `cross-vendor-review: passed` only when an
  independent reviewer from a different vendor returned no blocking findings.

## Don't touch (unless the task explicitly says so)

- `LICENSE`, `AGENTS.md`, `README.md`, `assets/`
- `docs/` (the orchestrator maintains the design wiki)
- `.github/` (except in CI tasks)
- `data/skills/compat.toml` (maintainer-curated)

## Code conventions

- Rust edition 2024. The toolchain is pinned in `rust-toolchain.toml`.
- Workspace layout and crate boundaries are fixed in
  [docs/plan.md](docs/plan.md#workspace-layout-decided-in-m0-then-fixed). Don't
  add a crate without the orchestrator's say-so.
- `unsafe` is forbidden. Libraries return typed errors (`thiserror`); the
  binary uses `anyhow`. No `unwrap()` / `expect()` outside tests.
- Add dependencies only when the task needs them, declare them in the
  workspace `[workspace.dependencies]`, and prefer widely used crates.
- Every public item has a doc comment. Tests sit next to the code
  (`#[cfg(test)]`) or in the crate's `tests/`.
- Pure logic (event fold, rules, projectors) stays free of I/O so it can be
  tested with fixtures.
- Commit messages: Conventional Commits (`feat(core): …`, `fix(git): …`,
  `test(views): …`, `chore: …`), imperative, one logical change per commit.

## Borrowed code

Borrowing is welcome, from MIT projects only, or from Apache-2.0 OR MIT projects
under their MIT option. When you adapt code:

1. Put a header in the file naming the source repo, path, commit and license.
2. Add the original copyright and license to `THIRD_PARTY_NOTICES.md` in the
   same commit.
3. Never copy from herdr-mobile-relay (AGPL) or herdr (Apache-2.0 only).

See [docs/licensing.md](docs/licensing.md).

## Working unattended

Nobody is watching a worker. Never stop on a question. When a choice is open,
take the option most consistent with your task contract and this file, carry
on, and state the assumption in your report.

## Orienting

The repo is indexed with CodeGraph. Use `codegraph explore "<symbol or
question>"` (or the CodeGraph MCP tool) before grepping. Read each file once.
Run long commands in the background with a log and poll them; never block on
one long foreground command.
