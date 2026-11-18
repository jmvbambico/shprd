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
  and the rule that a value equal to the inherited one is deleted, the
  self-drawn `frame:"none"` dialog with its page list, its row anatomy — a
  title, one line about it, the value and the scope line at the right, the box
  underneath — and its stepwise controls),
  `mods/shepherdr-settings/core/settings.js` (`SECTIONS`, `LEVEL`, `TITLE`),
  `lib/ui.js` (`link`, `segmented`, `toggleSwitch`, `openSettings`),
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
- Also used for: the Herdr desktop host folder in `host/herdr-desktop/` — the
  window's block tree and its measurements from `layouts/shepherdr.json`, the
  visual tokens (the text, height and radius scales, the red/green/amber status
  colours and the space marks) from `lib/ui.js`, the status glyph shapes from
  `lib/dot.js`, the status-bar, tab-bar, pane-header and footer rows from
  `mods/workspace-status-chat`, `mods/tabs-tasks`, `mods/pane-chat` and
  `mods/workspace-chrome-shepherdr`, the sidebar's rows — one 30 px line a space,
  its mark, its name and the red/green count badge at the end, the "Spaces"
  heading with its "+", the rail under 160 px, the row's right-click menu and the
  close confirm it asks in — from `mods/spaces-chat/main.js`,
  `mods/spaces-chat/sections.js`, `mods/spaces-chat/popups.js`, `core/menus.js`
  and `mods/confirm-chat/main.js`, and the split red/green badge itself (Shepherdr's
  `AttentionBadge`, `lib/ui.js`'s `countBadge`) in
  `host/herdr-desktop/mods/shprd-spaces/main.js`, the
  toast float and the numbers it follows — three at once, 360 px wide, a body cut
  at 140 characters, a dot per level, and a timeout per level with the older ones
  going first — from `mods/toasts/main.js` and `core/toasts.js`, the grouped
  two-line task rows and their second line from `mods/tasks/main.js` and
  `core/tasks.js`, the settings page's role rows, its picker button, its
  write-with-no-Save-step rule, its dialog frame, its page list and its level
  switch from `mods/shepherdr-settings/main.js`,
  `mods/shepherdr-settings/core/settings.js` and
  `mods/shepherdr-settings/settings.js`, the needs-you
  cards — a question, permission or plan as one card with its choices and the
  answer it waits on — from `mods/chat/main.js` and `core/asks.js` in
  `host/herdr-desktop/mods/{shprd-settings,shprd-chat}/main.js` and their
  `lib/{settings,asks}.js` (the cards are drawn in the chat's timeline since M11 U4), the Accept card's sections and folded Details, its
  summary rule and its Changes count line from `mods/accept-card/main.js` and
  `core/accept.js`, the task's pull-request card — the state word in a soft tint
  beside the number, and a quiet line when there is nothing — from
  `mods/task-pr/main.js`, and the task's head with its back link and switcher
  from `mods/chat/main.js`'s `head` branch, the web app's Chat screen (`web/src/agent/`: the bubbles, the show-more folding, the literal-markdown rule and the composer) from `mods/chat/main.js` and `core/chat-view.js`, the installer's
  ownership rules — own only what we wrote, never overwrite a file that is not
  ours, merge keys rather than replace them, back a change out on uninstall —
  from `bin/shepherdr` (`plan_files`, `merge_keys`) and `lib/UI.md`'s house
  rules, the window's own glyphs — the tab bar's spinner and close mark, the side
  panel's four icons and the tab, app and pane menus' row icons — copied whole
  from `mods/workspace-chrome-shepherdr/*.svg`, `mods/tasks/tasks.svg`,
  `mods/pane-chat/{split-right,split-down,zoom}.svg`,
  `mods/coordinator/rotate-ccw.svg` and `mods/keybinds/keyboard.svg` into
  `host/herdr-desktop/mods/{shprd-tabs,shprd-menu,shprd-new-menu}/icons/`, the
  status glyphs — the ring, tick, merging, queued, scheduled and dashed shapes
  and the 20 pie steps — from `mods/tasks/{circle,tick,merging,queued,scheduled,off}.svg`
  and `mods/tasks/pie/*.svg`, with the status bar's and the foot's `monitor`,
  `branch`, `plus`, `search`, `chevron-down` and `crook` from
  `mods/workspace-chrome-shepherdr/`, the pane header's `unzoom` from
  `mods/pane-chat/`, all into `host/herdr-desktop/mods/shprd-tabs/icons/`, and the
  status glyph's drawing, the pane header's hover buttons and "End <agent>?"
  question and the toasts' hover × and hover pause from `lib/dot.js`,
  `mods/pane-chat/main.js` and `mods/toasts/main.js`, and the
  mod-to-service discovery — `service.json`'s port and id, the
  `/api/health` id handshake before the token is sent, bearer auth — from
  `lib/local.js`, `lib/bridge.js` and `service/main.ts`, and Jump and Keybinds —
  the find box's groups, rows and keys from `mods/jump/main.js`, the grouped
  key list and its words from `mods/keybinds/{main.js,core/keys.js}`, the
  `keys.json` bindings from `keys.json`, and the `search.svg` icon from
  `mods/workspace-chrome-shepherdr/` — in
  `host/herdr-desktop/{layouts,mods,bin}/` and `crates/shprd/src/cmd/install.rs`
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
  fallback ladder — the error classes (also read in
  `crates/shprd-agents/src/exhausted.rs` to tell a spent account from a broken
  harness), the carry-on message and its waits, the record-tail reading of a
  cut, the one backup handover and its chat-tail brief — in
  `crates/shprd-watch/src/records/{mod,claude,codex,pi,sqlite}.rs`,
  `crates/shprd-watch/src/fallback.rs` and the events that record it in
  `crates/shprd-core/src/{event,state,rules}.rs`; and the role prompts — the
  shipped words per built-in role, the first-use write to
  `~/.config/shprd/prompts/`, the auto-upgrade of a file byte-identical to a
  retired default, an edit that is never overwritten, and the backup's
  takeover brief — in `data/templates/` and `crates/shprd-core/src/prompts.rs`;
  and the news a space's coordinator is told — the settle window, one line per
  change, one message per space and the re-check before sending — in
  `crates/shprd-views/src/news.rs` and `crates/shprd-server/src/news.rs`;
  and the herdr-core client — the `hello` framing switch, the request ids and
  the per-call timeout, the pending-call cap, the method and version gate with
  its refusal wording, the stream credit protocol, and the terminal event fold
  with its reset-on-`missed` discipline — in `crates/shprd-mux-hc/src/`; and the
  Herdr desktop host — terminal input sent with no `caller` so it arrives as the
  person's, the split between herdr-core for terminals and the window's own
  command line for spaces, tabs and panes, and making a space through the window
  — from `service/hub.ts` and `service/spaces.ts`, in
  `crates/shprd-mux-hc/src/host.rs`; and the chat's reading of an agent's own
  record — the line kinds each CLI keeps, the fields a message and its tool calls
  are read from, and the newest-page-then-what-it-gained shape — in
  `crates/shprd-agents/src/transcript.rs`; and the chat — the
  transcript's bubbles, markdown in a text view, "Show more" folding, the
  composer, the cards in the timeline and `literal` — in
  `host/herdr-desktop/mods/shprd-chat/` and `lib/chat.js`, and the pane header's
  Chat │ Terminal switch (`toChat`, `toTerminal`, the parked-agent label and the
  end-agent question) from `mods/pane-chat/main.js` in
  `host/herdr-desktop/mods/shprd-pane/`; and the tailnet share — the
  owner and machine name read from `status --json`, what `serve status --json`
  says about the mount, the refusal to serve where Funnel is on, never replacing
  a mount that points elsewhere, and the taken-HTTPS-port handling — from
  `service/tailscale.ts`, in `crates/shprd-remote/src/tunnel/{parse,tailscale}.rs`
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
