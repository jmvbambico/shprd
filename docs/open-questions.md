# Open questions

Things the design depends on that haven't been verified on a real machine.
Close each with evidence and move the result into the relevant page.

| Question | Affects | How to check |
|---|---|---|
| Does `prefix+alt` arrive at runtime on tuios in a real terminal? | [Multiplexers](multiplexers.md) | `tuios keybinds doctor` |
| Does tuios's `split-window` take a ratio? | layout | tuios protocol `list-verbs` |
| Does herdr-mobile-relay run on tuios's herdr-compat layer? | [Remote](remote.md) | compare the relay's socket calls with tuios's `unsupported` list; live test |
| Can a herdr pane declare itself an agent? | relay visibility (deferred) | live test |
| Is OpenCode/Kilo `SessionStatus.next` a timestamp or a delay? | [Availability](availability.md) | observe a real rate-limited turn |
| Does Codex load `~/.agents/skills`? | [Skills](skills.md) | `/skills` in a live Codex session |
| `tuios-web` pairing and auth on a phone | [Remote](remote.md) | live test |
| Will herdr-mobile-relay add `setup-link --json`? | remote box | ask upstream |
