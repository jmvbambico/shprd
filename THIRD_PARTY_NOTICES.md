# Third-party notices

## herdr-projects

- Project: herdr-projects
- URL: https://github.com/eliasstravik/herdr-projects
- Path: `docs/agents.md`
- Commit: a6787e1
- Used for: the per-CLI launch flag data in `data/harness/*.toml`
- License: MIT

MIT License

Copyright (c) 2026 Elias Stravik

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Shepherdr

- Project: Shepherdr
- URL: https://github.com/eliasstravik/shepherdr
- Paths: `mods/tasks/bin/herdr_task/agents.py` (`task_status()` and the
  helpers it uses), `core/tasks.js` (`place()` and `readyWait()`),
  `mods/tasks/bin/herdr_task/queues.py` (`queue_reason()`),
  `mods/tasks/bin/herdr_task/when.py` (`starts_words()`),
  `mods/tasks/bin/herdr_task/github.py` (`pr_body()` and the merge path),
  `mods/tasks/bin/herdr_task/launch.py` (`fresh_start()`, `CLEAN_ENV`),
  `core/agents.js` (`NAMES`, the permission mode tables, `PERM_ACROSS`,
  `argvFor` and its fast-mode values),
  `mods/tasks/bin/herdr_task/settings.py` (`argv_for`, `settings_layers()`,
  `settings_for()`, `task_settings()`),
  `core/settings.js` (`ROLES`, the role pick and level resolution,
  `otherCli()`), `mods/shepherdr-settings/main.js` (the per-scope level switch
  and the rule that a value equal to the inherited one is deleted),
  `mods/tasks/bin/herdr_task/provider.py` (`PROVIDER_WAITS`, `NUDGE`,
  `provider_cut`, `provider_error`),
  `mods/tasks/bin/herdr_task/takeover.py` (`backup_for`, `handover`,
  `takes_over`, `head`), `mods/tasks/bin/herdr_task/tick.py` (`retry_one`,
  `retry_pass`), `mods/tasks/bin/herdr_task/news.py` (`activity()`,
  `stall_of()`, `news_line()`, `news_state()`, `news_pass()`),
  `bin/shepherdr` (`HOOKS`), `mods/chat/ask-hook.sh`,
  `service/asks.ts`, `core/asks.js`,
  `mods/tasks/bin/herdr_task/templates.py` (`instructions()`, `OLD_WORKERS`,
  the first-use write and the upgrade of an untouched default),
  `templates/assistant.md`, `templates/worker.md`, `templates/reviewer.md`
- Commit: a3f327a (tag v3.1.34)
- Used for: the derived task status rules in
  `crates/shprd-views/src/derived.rs`, the per-task landing workflow —
  the fresh start a new worktree branches from, the merge policy and
  pull-request path, and the per-task pull-request body — in
  `crates/shprd-git/src/{land,worktree}.rs` and `crates/shprd-views/src/pr.rs`,
  and the agent-adapter facts — the Claude and Codex permission modes and the
  cross-program map between them, where an ask hook is installed and how
  long it waits, the launch argument order and the two fast-mode values, and
  the environment strip that keeps an agent from inheriting a parent Claude
  Code session — in `data/harness/{claude,codex}.toml`,
  `crates/shprd-agents/src/{spec,asks,adapter,env}.rs`; the ask file queue and
  waiting hook — one request file, one answer, never a decision by the hook
  itself — in `crates/shprd-core/src/ask.rs` and `crates/shprd/src/cmd/ask.rs`;
  also the settings model — the layered resolution and the layer each value
  comes from, the reserved built-in roles and the keyed role table, the
  per-repository entry and the "a value equal to the inherited one is stored
  nowhere" write rule — in `crates/shprd-core/src/settings/`; and the provider
  fallback ladder — the error classes, the carry-on message and its waits, the
  record-tail reading of a cut, the one backup handover and its chat-tail brief
  — in
  `crates/shprd-watch/src/records/{mod,claude,codex,pi,sqlite}.rs`,
  `crates/shprd-watch/src/fallback.rs` and the events that record it in
  `crates/shprd-core/src/{event,state,rules}.rs`; and the role prompts — the
  shipped words per built-in role, the first-use write to
  `~/.config/shprd/prompts/`, the auto-upgrade of a file byte-identical to a
  retired default, an edit that is never overwritten, and the backup's
  takeover brief — in `data/templates/` and `crates/shprd-core/src/prompts.rs`;
  and the news a space's coordinator is told — the settle window, one line per
  change, one message per space and the re-check before sending — in
  `crates/shprd-views/src/news.rs` and `crates/shprd-server/src/news.rs`
- License: MIT

MIT License

Copyright (c) 2026 Elias Stravik

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
