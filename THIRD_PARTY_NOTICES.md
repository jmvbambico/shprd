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
  `mods/tasks/bin/herdr_task/launch.py` (`fresh_start()`),
  `core/agents.js` (`NAMES`, the permission mode tables, `PERM_ACROSS`,
  `argvFor`), `mods/tasks/bin/herdr_task/settings.py` (`argv_for`),
  `bin/shepherdr` (`HOOKS`), `mods/chat/ask-hook.sh`, `service/asks.ts`,
  `core/asks.js`
- Commit: a3f327a (tag v3.1.34)
- Used for: the derived task status rules in
  `crates/shprd-views/src/derived.rs`, the per-task landing workflow —
  the fresh start a new worktree branches from, the merge policy and
  pull-request path, and the per-task pull-request body — in
  `crates/shprd-git/src/{land,worktree}.rs` and `crates/shprd-views/src/pr.rs`,
  and the agent-adapter facts — the Claude and Codex permission modes and the
  cross-program map between them, and where an ask hook is installed and how
  long it waits — in `data/harness/{claude,codex}.toml`,
  `crates/shprd-agents/src/spec.rs` and `crates/shprd-agents/src/asks.rs`,
  and the ask file queue and waiting hook — one request file, one answer,
  never a decision by the hook itself — in `crates/shprd-core/src/ask.rs` and
  `crates/shprd/src/cmd/ask.rs`
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
