# Commands

Status: built through M11. `shprd up`, `shprd down`, `shprd remote` and `shprd keys` are
described below but not built yet (remote access is `shprd web`, below); the setup wizard was dropped in the M7 re-plan.
Today a run starts with `shprd run start` and `shprd serve` drives it; on the Herdr
desktop, `shprd install herdr-desktop` puts in the layout and mods that show it.

## For you

| Command | Does |
|---|---|
| `shprd up` | start or resume this repo's run |
| `shprd down` | finish the run; refuses (exit 2, nothing written) while any task is unfinished or the run's pull request has not merged, and names each task and how to proceed: `shprd up` restarts the orchestrator or watchdog and resumes this run. `shprd down --abandon` (you only, never an agent or the orchestrator) asks you to confirm at a terminal, naming the run and each task it would leave with its branch; a 'no' or a missing terminal writes nothing and closes nothing. On a 'yes' it ends the run anyway, records which tasks were left (with an unmerged pull request, the integrated tasks it carried too), and keeps their branches. The orchestrator may end a run normally once every task is integrated and the pull request merged |
| `shprd run start --run-id <ID> --base <BRANCH> --integration <BRANCH>` / `shprd run finish` | start a run (the first event in the log) / finish it. The `--integration` branch must already exist (`git branch integration/m<N> main`); `run start` does not create it yet |
| `shprd serve [--port N]` | the service: a loopback HTTP API for every UI, plus the tick every `tasks.tick_secs`. See [The service](#the-service) |
| `shprd install herdr-desktop [--dry-run] [--uninstall] [--from DIR] [--repo DIR] [--app-home DIR]` | put shprd's layout, its `shprd-*` mods and their keys into the Herdr desktop's home (`~/.config/herdr-app`, or `$HERDR_APP_HOME`). It owns only the files it wrote (a manifest with a SHA-256 of each), refuses and writes nothing on a clash with a file it did not write, never overwrites a key you bound, and puts back a layout Herdr pruned. The files come from `--from`, `$SHPRD_HOST_DIR`, the current checkout, or the copy built into the binary, in that order (`--repo` names the checkout whose `host/herdr-desktop` to use when the working directory is not it). A file of a mod this build no longer ships is taken back like any other file it wrote, and a layout an earlier build shipped, which Herdr pruned a terminal block from, is put back rather than refused |
| `shprd tick [--once] [--json]` | run one pass of the service's tick by hand: start, review, verdict, send back, escalate, land |
| `shprd settings show\|get\|set\|unset` | the settings: roles (`roles.worker.agent`, `roles.reviewer.model`, …) and workflow keys (`tasks.merge`, `tasks.max_running`, …), each printed with the layer it came from. `set` refuses a reviewer of the worker's model family |
| `shprd prompts list\|show\|path\|reset <id>` | the prompts each role starts with, stored under `~/.config/shprd/prompts/`; your edits are kept, `reset` brings back the shipped words |
| `shprd` / `shprd status` | tasks, agents, quota, and what needs you |
| `shprd brief` | a short, paste-able summary of the run |
| `shprd remote [--invite]` | not built. Remote access is `shprd web`, below |
| `shprd web password set` / `clear` | set or forget the login password every tunnel's browser must enter. `set` asks twice at this terminal with no echo (or reads two piped lines); at least 12 characters, not a very common password, not this machine's name. Both end every session. Local only: no web route can set or reset it, so the loopback token is the way back in after `clear` |
| `shprd web sessions [--revoke ID \| --revoke-all]` | list the browser sessions (the first 12 characters of each stored hash, which is the id `--revoke` takes; never a cookie value), or end one or all. The service sees a revoke at once |
| `shprd web up [--tunnel P] [--stable] [--show-url]` | put the web app on a tunnel. `P` is `tailscale`, `cloudflared`, `cloudflared-named`, `ngrok` or `tunnl`; without it the `web.tunnel` setting decides (`tailscale` by default). `--stable` (tunnl.gg only) keeps the same address, tied to your SSH key (else the `web.stable` setting). Asks the running `shprd serve` to start it, so it says so and does nothing when none runs; the tunnel is the service's child and ends with it. Every provider but `tailscale` refuses to start until a password is set, and there is no flag around that. The providers are your own programs from `PATH`; nothing is downloaded. The link is masked unless `--show-url`, which also prints its QR code (the link is the address of a login page that reaches a shell: do not paste it in a chat) |
| `shprd web down` | stop the tunnel |
| `shprd web status [--show-url]` | whether a tunnel is up, the provider and the masked link. Reads `web.json`, so it needs no service; a record left by a killed service shows as stale |
| `shprd gate` | run the repository's full checks (`[gates] integration`) once on the integration branch |
| `shprd sync` | merge the base branch into the integration branch when it moved under the run (a pull request merged meanwhile): a merge commit, never a rebase, so task commits keep their ids; a conflict is undone, escalated with the files, and exits 2. Landing needs a fresh `shprd gate` afterwards. The orchestrator runs it before `shprd gate`; no terminal needed |
| `shprd land` | push the integration branch and open or update the pull request; when the base has moved it says so and asks for `shprd sync`, then `shprd gate` |
| `shprd land --merge` | merge it: only with green checks, the reviewed head, and your approval |
| `shprd approve` | approve the open pull request at the integration branch's current head, for a solo maintainer who can't approve their own PR on GitHub; humans only, needs a terminal, and a new commit needs a new approval |
| `shprd readmit <agent> [--reason TEXT]` | clear a dry agent's mark so the run may spawn and hand off to it again (`shprd status` names the agent to pass); humans only, and unlike `approve` it needs no terminal |
| `shprd override --task <ID> --rule <RULE> --reason <TEXT>` | set a rule aside for one task. One rule honours it today: `integrate_after_failed_gate` lets the task integrate although a gate failed, a review included. Humans only, and it needs a terminal: the question names the task, the rule and the failed gate or review, and a 'no' or a missing terminal writes nothing. An orchestrator or an agent cannot record one, so the work a review stopped cannot be pushed through without you, and `shprd land` marks the pull request `degraded-review` when a failed review was overridden |
| `shprd watch` | the fallback loop: nudges stuck agents and hands off rate-limited ones. The service's tick records review verdicts, so `watch` is not needed for a run to finish |
| `shprd janitor [--apply]` | show (or apply) cleanup of what the run created: worktrees, branches, the merged remote branch |
| `shprd keys show\|install\|uninstall` | the `prefix+alt+…` key bindings in your herdr config |
| `shprd doctor` | check CLIs, sign-ins, skill visibility, mux, relay, git and gh; judges the roles and the cross-vendor pair from the settings |
| `shprd set orchestrator\|reviewer <agent> [--effort E]`, `shprd set workers <agent>…` | change the roster in your global config, or the repo's with `--repo <path>`; e.g. `shprd set reviewer codex/gpt-6-luna --effort high` |
| `shprd roster` | show agents, roles, models, availability |
| `shprd drop <task> --reason TEXT` | drop a task you decided not to do, so it stops blocking the run: it asks you at a terminal, naming the task, its status and its branch; a 'no' or a missing terminal writes nothing. Humans only. Nothing is deleted now — the worktree and branch stay, the janitor archives the branch before it removes either, and `shprd land`'s pull request lists the task and your reason under `## Dropped` |
| `shprd restore <task>` | bring back archived work |

Every command supports `--json`. Shell completion (`shprd completion zsh`) is planned, not built yet.

Agent and model strings use the CLI's own model names: `program/model`, e.g.
`opencode/deepseek-v4.1-flash-free` or `claude/claude-opus-5-5`, with effort set
separately (`--effort high`). Pi's models must name their provider, because the
same model id can be served by several providers:
`pi/Command Code/deepseek/deepseek-v4.1-flash`.

### How `shprd set` writes your config

- The change is validated in memory first; an invalid change writes nothing and
  exits 2 (for example a reviewer equal to the orchestrator).
- Comments and other keys in the file are kept.
- The new file is written to a temporary file, synced, and renamed into place, so
  the config is never half-written.
- Two `shprd set` runs never interleave: they share a lock file
  (`.config.toml.lock` next to the config). A second run while one is active
  exits 2.
- If the file changes while `set` is working, `set` refuses (exit 2) instead of
  overwriting. The one gap: an editor that ignores the lock file and saves in the
  instant between that final check and the rename can still be overwritten. No
  POSIX tool can close that window; editors such as vim have the same one.

## For agents

Hidden from the main help. The orchestrator and workers call these, and the
engine validates every one:

| Command | Who | Does |
|---|---|---|
| `shprd agent task add` | orchestrator | define a task: title, scope, acceptance, tags, deps |
| `shprd agent spawn --task` | orchestrator | start a task (or its next attempt) on the next available worker, in its own worktree and tab |
| `shprd agent prompt --task` | orchestrator | send a message to a task's agent |
| `shprd agent report` | worker | submit status, tests, checks, changed paths, commit, skills used, and `--proof <path>` (never in the summary); one hand-in per attempt |
| `shprd task status <ID> --msg TEXT [--progress N]` | worker | record a short progress line |
| `shprd ask raise --kind question --text TEXT [--option O]…` | worker | raise a needs-you card yourself (programs with no ask hook, such as pi and opencode) and block until the person answers; prints the answer |
| `shprd ask hook` / `shprd ask install` | program hook / engine | the ask hook Claude Code and Codex call, and its install into the task worktree's own config (never your global config) |
| `shprd agent review --task [--degraded]` | orchestrator | open a reviewer of a different model family on the task's diff |
| `shprd agent review --task <ID> --again` | orchestrator | restart a stuck review (brief never arrived, reviewer hung): fresh reviewer tab, same attempt; refused once a verdict exists |
| `shprd agent review-verdict --task --attempt` | engine | record the verdict from the reviewer's own `findings.json`; code decides pass or fail. The service's tick runs it; anyone else is refused |
| `shprd agent integrate --task` | orchestrator | cherry-pick a passed task onto the integration branch |
| `shprd agent handoff --task` | engine (`watch`) | move a task to the next worker, keeping its work |
| `shprd agent escalate` | any | non-blocking note for a human, always with a stated default |

## The service

On the Herdr desktop nobody types `shprd serve`: the window starts the one service and
registers each space the person adds. A folder that is not a git repository is served too; at the person's
first chat message the service makes it one (`git init` on `main`, a `.shprd/config.toml`
with the test commands it found, an ignore for the run's files, and a first commit of what
was there) and says so in one plain line in the chat. A folder inside a repository serves
that repository. Showing a folder never makes a repository. The first task starts the run
(base: the checked-out branch, integration branch `integration/r<time>`), so no
`shprd run start` is needed either.

`shprd serve` binds 127.0.0.1 only. There is **one service per user**, for every
repository: one state directory (`$XDG_STATE_HOME/shprd`), one port, one token and
one `service.json` (port, id, pid, version). A client finds it there and reads the
port from the record rather than guessing. On a clean stop it removes `service.json`.
The same folder holds the service's remote files: `web-auth.json` (the password's
argon2id hash), `sessions.json` (the SHA-256 of each session id, never the id) and
`web.json` (the tunnel's record), so `shprd web` works from any folder. All are
private to you (mode 0600). A state directory allows one service at a time.

Each repository the service serves has its own run, event log (`.shprd/` in the
repository), tick, news and coordinator, and a directory of its own,
`<state>/repos/<name>-<hash>/` (`<name>` is the folder's name, `<hash>` an FNV-1a hash of
its full path), holding its `changes` file and last pass. That name is the repository's
stable id in the API. A repository is **registered** with the running service:
`POST /api/repos {folder}` (idempotent, answers `{id, root, dir}` at once and prepares
the repository's space and coordinator in the background), `PUT /api/repos {folders}`
(exactly the layout's spaces: new ones are registered, every other repository stops
being ticked and is dropped, and its coordinator is left idle where it is),
`DELETE /api/repos {folder}`, and `GET /api/repos`. The three writes take the token
door only, never a web session. `shprd serve --repo <path>` (or started inside a
repository) registers that repository before it answers, and never drops it.

Every repository-scoped route (`state`, `changes`, `asks`, `chat`, `terminal`, `action`,
`graph`, `tasks`, `settings`, `coordinator`, `agents`, `ws`) names its repository with
`?repo=<id or path>` (or the `x-shprd-repo` header). Only a registered repository
resolves, and the value is never opened as a path. A request that names none is
answered by the only repository registered, and refused in words when there are none
or several. Health, login, sessions, tunnels, `roster` and `prompts` are the
service's own and name no repository.

Starting the one service: the first `shprd serve` after an upgrade stops the
per-repository services an earlier version ran (only a pid its own record names, and
only while that pid is still a `shprd serve`), removes their records, and keeps one
web password (the newest) as the service's own; the others are dropped and
`shprd web password set` asks again.

The Herdr desktop starts the service itself, once, when its layout loads (a core
terminal named `shprd-service`, kept across space switches and window restarts), and
tells it every space's folder. Switching spaces only changes which repository the
panels read: nothing is started or looked for again. The footer says
`shprd: service up · <repository>`, or the reason in plain words when a start fails.
Nobody types `shprd serve`.

The web app (`web/dist`, embedded in the binary) is served from the same
listener: any path that is not `/api…` is an asset, or `index.html` for an
extensionless path. A binary built without `web/dist` still serves the API and
says how to build the app.

| Route | Does |
|---|---|
| `GET /api/health` | `{ok, id, version}`; needs no credential |
| `POST /api/login` | `{password, remember?}`; starts a session and sets its cookie (`remember`: 30 days idle instead of 12 hours); with a session already, it enters the password again to make it fresh (`refreshed: true`). One failure message for a wrong, an absent or an expired credential; needs no credential, but is rate limited |
| `POST /api/logout` | end this session |
| `GET /api/session` | who is asking: `via` (token or session), whether the password is still fresh, whether a password is set at all, and the session's id, times and label |
| `GET /api/sessions`, `DELETE /api/sessions/{id}` | the live sessions and which one asked / end one (by the 12-character id) |
| `GET /api/tunnels`, `POST /api/tunnels/start`, `POST /api/tunnels/stop` | the tunnel as `shprd web status` shows it (provider, link, QR as SVG, whether a password is set, every provider, and `tailnet_user` on a request that came through Tailscale serve) / start one (`{provider?, stable?}`; a session needs a fresh password) / stop it |
| `GET /api/state` | the snapshot: tasks with their derived status (`Needs you`, `Working`, …), `needs`, `asks`, agents, `run` (null with a `hint`, "no tasks yet: say what you want in the chat", when there is no run yet), `last_tick` (the last pass's steps and failures), `spaces` (one per repo: id, name, folder, branch, and Herdr's own `herdr_id` and `herdr_name` when the host has them) and `live_agents` (each worker, reviewer or coordinator the host lists: id, space, task, role, program, state `working`, `needs_you`, `idle` or `done`, its terminal id, and start, end and since times) |
| `GET /api/changes`, `GET /api/ws` | what changed since a seq / the same pushed over a socket. A browser cannot set headers on a socket handshake, so `/api/ws` alone also takes the bearer token as `?token=` |
| `GET /api/asks`, `POST /api/asks/answer` | open needs-you cards / answer one (`{task, ask, answer}`) |
| `POST /api/action` | the person's actions on a task (`task.add`, `status`, `start`, `retry`, `stop`, `drop`, `drop_many`, `accept`, `resend_brief`). `task.drop_many` (`{tasks, reason}`) drops several tasks with one reason, skipping the ones the rules refuse and naming them in `skipped`. `task.drop` and `task.drop_many` are destructive: through a session it needs a password entered in the last five minutes, else `401` with `code: reauth` |
| `GET\|PUT /api/settings` | the settings verbs over HTTP |
| `GET /api/prompts`, `GET\|PUT\|DELETE /api/prompts/{id}` | the prompts: list, read, write, reset to the shipped words |
| `POST /api/tasks` | add a task, as `shprd agent task add` does |
| `GET /api/tasks/{id}/changes` | the task's branch against the branch it was cut from (the integration branch), for the accept card |
| `GET /api/chat?space=&since=`, `POST /api/chat` | what a space's agent wrote since a point, read from the program's own transcript (Claude Code, Codex, Pi, OpenCode) / type one turn into it, as the person (`{space, text}`; the answer's `notice` is one plain sentence about what shprd did first, such as making the folder a repository). `space` is `coordinator` or a task id; absent means the coordinator |
| `GET /api/terminal?space=&pane=` | upgrade to a WebSocket on one agent's terminal. `space` is `coordinator`, a task id or `agent:<id>` (a listed agent); a `pane` must equal the one the service resolves. It watches by default (the screen is pushed whole when it changes; the pane is never resized) and types only after the client sends `input_open`. Messages: server `hello`, `screen`, `input_ready`, `notice`, `ended`, `pong`; client `input_open`, `input`, `key`, `ping`. At most 8 sockets open, 200 client messages per 10 s per socket, 16 KiB per input; nothing typed is logged |
| `GET /api/agents/{id}/output?lines=N`, `POST /api/agents/{id}/attach` | a listed agent's recent screen, read-only (the fallback for a client that cannot bind a terminal; a finished agent's stays readable) / what to bind a real terminal to (its terminal id and its socket path) |
| `POST /api/agents` | start a program in a new tab, in the run's space (the "+" menu's *Start an agent*) |
| `GET /api/graph`, `GET /api/graph/run` | where dagr is (`$SHPRD_DAGR_BIN`, the `graph.dagr` setting, `PATH`, then the herdr-dagr plugin folder), which probe found it, and the command that draws the run / the run document (`.dagr/run.json`) verbatim, which the web app draws itself; `404` before the first one is written |
| `GET /api/roster` | the programs a role may run, with their models and effort levels |

Every route but health and login passes the gate, in this order:

1. **`Host`** must be a name the service answers to: `127.0.0.1`, `localhost`,
   `[::1]`, or the host of the tunnel that is up (added on start, removed on
   stop). Anything else is `421`, on every route, health and login included.
2. **A credential** by either door: the bearer token (the mods and the CLI, from
   loopback) or a session cookie (a browser that entered the password). Missing
   or no longer valid is `401`.
3. **A write or a socket upgrade** also needs an `Origin` equal to the page's own
   origin (the token door's, when it sends one, must be loopback), and `x-shprd: 1`
   (not on a socket upgrade, where the browser sets the handshake and `Origin`
   is the rule). Refused with `403`.
4. **Rate limits**, keyed on the peer address (`X-Forwarded-For` is ignored):
   `429` with `Retry-After`. See [Remote access](remote.md#rate-limits).

The tick (every `tasks.tick_secs`, at most `tasks.max_running`
tasks at once) starts queued tasks whose deps landed, starts the reviewer role on a
hand-in, records the verdict, sends a failed task back while review rounds are
left and escalates to a needs-you card when they are not, lands what
`tasks.merge` allows (asked about the branch the landing writes, not the base),
and closes the landed task's agents. A failed step is logged on stderr at the level
`SHPRD_LOG` sets (`warn` by default) and kept in `last_tick`.

At startup `serve` creates or reuses the repo's one space, labelled exactly
`shprd · <repo folder name>`, and starts the coordinator agent in it (or finds it
running after a restart). A task gets no space and no tab: its worker and reviewer
run as agents the host lists, headless, and show in `live_agents`. When the
checkout is under the temporary directory and the socket is not, `serve` makes no
space (a live check must never reach your own Herdr) and says so in its banner;
the API still serves. The graph is no longer a terminal the service starts.

`serve` picks its host from the `host` setting: `auto` (the default: the Herdr desktop
when its `herdr-core` answers, else herdr; never tuios), `herdr-desktop`, `herdr` or
`tuios`. A change to `tasks.tick_secs` or `tasks.max_running` takes effect on the next
pass, without a restart. A `serve` whose lock was left by a crashed one starts anyway;
a live one is refused. The mods follow a restarted service, on the same port or a new
one.

The remote settings are `web.tunnel` (the provider `shprd web up` uses without
`--tunnel`), `web.stable`, `web.cloudflared_name` and `web.cloudflared_host` (the
last two name a `cloudflared-named` tunnel and its hostname). A repository's
committed settings file cannot set any `web.*` key: where a shell is exposed is
your decision, not a cloned repo's.

## Config sections added in M5 and M6

```toml
[gates]
integration = ["cargo fmt --all --check", "cargo test --workspace"]  # run by `shprd gate`

[review]
must_fix = "major"   # findings this severe or worse fail the review
rounds = 2           # failed reviews before shprd escalates to you

[land]
merge = "ask"        # "ask": merge only after your approval (GitHub or `shprd approve`); "checks": also once the checks pass; "auto": also without waiting, never into a protected branch
protected = ["main", "master"]
```

Reviewers never run with auto-approve. A harness spec can confine a reviewer's
writes with `reviewer_args` (Codex: `--sandbox workspace-write`, so it can only
write inside its review folder); other CLIs ask you before their reviewer uses a
tool.
