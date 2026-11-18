# Remote access

Status: built in M11 (the crate `shprd-remote`, the routes in `shprd-server`,
the `shprd web` commands and the web app's Settings › Remote). The commands and
routes are in [Commands](commands.md); this page is the model behind them.

The remote surface is the **web app**: `shprd serve` serves it on the loopback
port, and a tunnel puts that port on an address a phone or another machine can
reach. It is the same app and the same API for the desktop browser and the
phone. shprd's own login guards it, whichever tunnel carries it.

The Herdr desktop's mods are not remote: they talk to the service on loopback
with its bearer token, as before.

## Providers

One tunnel runs at a time, as a child of `shprd serve`. It ends when the service
does, on `shprd web down`, and after a storm of failed sign-ins (the global
login limit or the account lock below). The providers are your own programs, from
`PATH`: shprd downloads and installs nothing, and prints the version of the one
it runs.

| Provider | Word | Runs | Reached by |
|---|---|---|---|
| Tailscale serve | `tailscale` | `tailscale serve` | a private name on your tailnet; never Funnel |
| Cloudflare quick tunnel | `cloudflared` | `cloudflared tunnel --url` | a new random `trycloudflare.com` name each run; no account |
| Cloudflare named tunnel | `cloudflared-named` | `cloudflared tunnel run <name>` | your own hostname; set `web.cloudflared_name` and `web.cloudflared_host` first |
| ngrok | `ngrok` | `ngrok http <port>` | the address ngrok's local agent reports |
| tunnl.gg | `tunnl` | `ssh -R 80:… proxy.tunnl.gg` | a random address, or with `--stable` / `web.stable` one tied to your SSH key |

`shprd web up --tunnel <word>` starts one; without `--tunnel` the `web.tunnel`
setting decides (`tailscale` unless you changed it). The same is on Settings ›
Remote in the web app (`POST /api/tunnels/start`).

**A provider on the public internet does not start unless a login password is
set.** That is every provider but `tailscale`. There is no flag around it.
Tailscale is private to your tailnet, so it may start without one, but the
login still guards the app: a request through it has no credential of its own,
and the tailnet's identity (`Tailscale-User-Login`, believed only on the
tailscale tunnel's own host) is shown, not trusted as a login. Nothing else a
tunnel's edge adds (`X-Forwarded-For`, `Cf-Access-*`) is believed.

## The password and sessions

- The password is set at a terminal, never over the web:
  `shprd web password set`. A web app is the shell a tunnel exposes, so it can
  never reset the credential that guards it. At least 12 characters, not one of
  the commonest passwords, not this machine's name. It is stored as an argon2id
  hash (64 MiB, three passes) in `web-auth.json`. With no password set, a login
  is refused after the same work, so a missing password never looks like an open
  door.
- A login (`POST /api/login`) starts a **session**: a random id that lives only in
  the browser's cookie (`__Host-shprd` over https, `shprd` on plain loopback;
  `HttpOnly`, `SameSite=Lax`). The service stores only the SHA-256 of it.
- A session lasts until it is idle for 12 hours, or 30 days with "remember this
  device", and at most 90 days either way.
- `shprd web sessions` lists them, `--revoke <id>` or `--revoke-all` ends them,
  and the service sees that at once. Setting or clearing the password ends every
  session. After `clear` nothing can sign in through the web; the loopback token
  is the way back in.
- The loopback bearer token stays a second door, for the mods and the CLI. A
  browser never sees it.

## The gate

Every route but health and login goes through one gate, a pure function in
`shprd-remote` with its decision table under test.

| Check | Refusal |
|---|---|
| `Host` is a loopback name or the running tunnel's host (the allowlist) | `421`, on every route including health and login; a wrong `Host` is what DNS rebinding looks like |
| a bearer token, or a valid session cookie | `401` |
| on a write or a socket upgrade, `Origin` is the page's own origin (the token door's must be loopback) | `403` |
| on a write, the header `x-shprd: 1` (a form post cannot set it; a socket upgrade relies on `Origin`) | `403` |
| a destructive action through a session needs the password entered in the last five minutes | `401` with `code: reauth`; the loopback token is exempt |
| the rate limits | `429` with `Retry-After` |

Today the destructive action is `task.drop`, and starting a tunnel from the web
needs the same fresh password, so a stolen cookie cannot put the machine on a new
public address.

### Rate limits

Keyed on the peer address; a header a client writes is not an address.

| Limit | Value | On breach |
|---|---|---|
| login attempts per address | 5 per 15 min, then blocked 15 min, then 1 h, then 6 h | `429` |
| login attempts, all addresses | 30 per hour | `429` |
| wrong passwords for the account | 10 per hour, then locked for 1 h | `429` |
| new sessions per address | 5 per hour | `429` |
| writes per session | 60 per minute | `429` |
| unauthenticated requests per address | 120 per minute | `429` |

The local CLI is untouched by these: it is your way back in.

## The link and the QR

`shprd web up` prints the link masked. `--show-url` (and `shprd web status
--show-url`) prints the link and its QR code in the terminal; Settings › Remote
shows the same QR as an SVG to someone signed in. For a quick tunnel the address
is the one thing between the internet and a login page, so it is never logged in
full and never in a reply to anyone who is not signed in. Do not paste it in a
chat. The QR carries only the address: the password is still typed on the phone.

## herdr-mobile-relay

On the herdr TUI, [herdr-mobile-relay](multiplexers.md) can still do phone and
browser access with its own pairing. It is AGPL, so shprd only installs it from
upstream on request and calls it; it never bundles or patches it. It is separate
from the web app and its login, and is not part of M11.

## tuios

tuios has its own remote surfaces: `tuios-web`, SSH server mode, `hosts`, and
`register-push`. Their pairing and auth model is unverified, and the web app's
login does not cover them. **herdr-mobile-relay may also run on tuios**, since
tuios hosts herdr plugins and serves a herdr-compatible socket when
`[agents] herdr_protocol = "always"` is set (see
[Multiplexers](multiplexers.md#tuios-speaks-herdr)). That gets tested in M12.
