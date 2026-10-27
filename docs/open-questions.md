# Open questions

Things the design depends on that haven't been verified on a real machine.
Close each with evidence and move the result into the relevant page.

| Question | Affects | How to check |
|---|---|---|
| Is the `prefix+alt+<letter>` layer free on herdr and tuios? | [Multiplexers](multiplexers.md) | `herdr --default-config`; `tuios keybinds free` |
| Which environment variable marks "inside tuios"? | launch | tuios source or a live pane |
| Does tuios's `split-window` take a ratio? | layout | tuios protocol `list-verbs` |
| Can tuios host herdr plugins (`plugin_action`)? | [Remote](remote.md) | tuios source; a live test with herdr-mobile-relay |
| Can a herdr pane declare itself an agent? | relay visibility (deferred) | live test |
| Do OpenCode and Kilo expose retry status through `serve`? | [Availability](availability.md) | `opencode serve` OpenAPI and event stream |
| Exactly how og reads Claude and Codex quota | [Agents](agents.md) | og source |
| Does Codex load `~/.agents/skills`? | [Skills](skills.md) | `/skills` in a live Codex session |
| `tuios-web` pairing and auth on a phone | [Remote](remote.md) | live test |
| Will herdr-mobile-relay add `setup-link --json`? | remote box | ask upstream |
