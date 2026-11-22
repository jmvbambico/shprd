G5 proof (no live claude window; see the hand-in summary for why):
- settle-hook-pty.txt: the hook command shprd now installs for Claude, run as a child of a pty,
  writes ESC]7501;state=working ESC\ to that terminal (what hc reads as the status).
- workspace-tests.log: cargo test --workspace; the shprd-mux-hc host/client and shprd serve
  tests that fail under load pass when run alone (mux-hc with --test-threads=1).
- unit tests: asks::tests::claude_settles_a_blocked_status_once_the_prompt_is_answered and
  spec::tests::claude_settles_a_blocked_status_without_touching_the_open_prompt.
