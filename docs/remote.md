# Remote access

Status: design.

## herdr: herdr-mobile-relay

The relay already does phone and browser access well:

- QR pairing;
- approvals as cards;
- prompts, diffs and worktrees;
- Cloudflare tunnel, community gateway, own gateway or LAN, all end-to-end
  encrypted.

shprd does not reimplement any of it. It's AGPL, so shprd only installs it from
upstream on request and calls it; it never bundles or patches it.

The phone sees real agents: the orchestrator tab and the worker tabs.
`shprd·chat` isn't registered as an agent, so it doesn't appear there.

Bring-your-own tunnels (ngrok, tunnl) are not supported. The phone app is served
from its own origin, and pairing is bound to that origin. A TLS proxy can't
carry the relay's UDP paths either.

## The remote box

The box sits under dagr in `shprd·chat` and is toggled with `prefix+alt+r`.

The relay's QR and setup link **are credentials**: a one-use invitation, valid
for ten minutes from each presentation until a device enrols. So:

| State | Shows |
|---|---|
| collapsed (default) | one line: relay up, transport, paired devices, connected count |
| expanded | relay host and device list (nothing secret) |
| invite (explicit key `i`) | fresh QR and link with a visible 10:00 countdown; hides after the first enrolment or at 0:00; the secret part of the link is masked until you copy it |

The relay has no machine-readable "give me the link" command; its CLI only has
`version --json`. For now the box runs the relay's own `setup-link` action
inside itself. We'll ask upstream for a `--json` option.

## tuios

tuios has its own remote surfaces: `tuios-web`, SSH server mode, `hosts`, and
`register-push`. On tuios, the box shows the `tuios-web` address and status.
Its pairing and auth model is unverified.
