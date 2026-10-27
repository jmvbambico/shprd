# Decisions

Newest first. Each entry: what was decided, and why. Pages hold the current
truth; this log holds how we got there.

## 2026-10-27

- **Release on every merge after v0.1.0.** Each approved PR merged into `main`
  gets the next semver tag, computed from Conventional Commits, and a published
  GitHub Release with binaries. Tag-only (no bump commit), so protected `main`
  needs no bypass. See [plan.md](plan.md#releases-after-v010).
- **Build plan proposed** ([plan.md](plan.md)): M0–M9 with gates. The dogfood
  point is after M6, when shprd builds shprd.
- **Keys confirmed free:** `prefix+alt+<letter>` has no default binding on
  either herdr or tuios.
- **tuios speaks herdr** (plugins, socket, CLI shim). M8 starts by trying the
  herdr adapter and herdr-mobile-relay on it before writing tuios-native code.
- **Language: Rust.** The projects we're most likely to borrow from (herdr-cadence,
  herdr-projects, herdr-dagr) are Rust.
- **License: MIT.** It's compatible with everything shprd might borrow, and it
  isn't misleading about code that keeps upstream MIT conditions. Replaced an
  initial WTFPL.
- **Docs live in `docs/` as a wiki**, starting at [docs/README.md](README.md).
- **Tabs:** tab 1 is `shprd·chat`, which isn't an agent; tab 2 is the real
  orchestrator CLI, visible. Tab names are role symbol + `cli·task`, never
  `T<n>`. See [Roles and tabs](roles.md).
- **Keys:** `prefix+alt+<letter>` on both herdr and tuios. Plain prefix letters
  collide differently on each. See [Multiplexers](multiplexers.md).
- **Skills:** global skills (`~/.agents/skills`, via `npx skills`) are routed
  and curated; CLI-native skills are honoured as-is. Curating npx skills for
  compatibility is a maintainer commitment. See [Skills](skills.md).
- **First-release CLIs:** claude, codex, opencode, kilo, pi.
- **Chat pane direction: Transcript.** Calm single column; engine facts appear
  as cards rendered from events.
- **The orchestrator runs as a real CLI**, and `shprd·chat` reads its
  conversation record. A headless orchestrator would give a cleaner stream, but
  the phone relay couldn't see it.
- **Own engine, two hosts.** herdr-projects and herdr-cadence are tied to herdr,
  so supporting tuios requires shprd's own engine with a thin adapter per
  multiplexer.
- **The engine owns state.** An append-only event log; every view (dagr, brief,
  PR body) is rendered from it by code. No LLM writes status files.
- **Integration and landing are engine steps, not LLM roles.** Pushing, opening
  a PR and pruning are deterministic. Conflict repair goes back to a worker.
- **Remote stays host-specific.** herdr-mobile-relay on herdr; tuios's own web
  and SSH access on tuios. Porting the relay would mean maintaining an AGPL fork.
- **Leaving og.** shprd replaces og (Omnigent) with a pure-TUI harness. It keeps
  og's workflow without og's setup burden.
