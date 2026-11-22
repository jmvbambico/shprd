G5 proof. No live claude window was opened: a private Herdr window with a real claude permission prompt needs a signed-in claude and a model call, which a worker must not make here.
- asks::tests::a_blocked_claude_stops_needing_you_once_the_prompt_is_answered (shprd-agents): installs the hooks into a coordinator and a worker worktree, runs the real PostToolUse and Stop hook commands against a stand-in terminal, and folds their output with the OSC 7501 parser: blocked -> working / idle, never Needs you; the PermissionRequest hook reports nothing, so an open prompt stays blocked.
- settle-hook-pty.txt: the hook command run under a real pty writes ESC]7501;state=working ESC\.
- workspace-tests.log: cargo test --workspace --no-fail-fast.
- Codex, Pi, OpenCode: pi/opencode have no ask hook; codex's only hook is PermissionRequest and sets no status (noted in data/harness/claude.toml).
