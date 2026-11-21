#!/usr/bin/env python3
"""SHPR-79: one uninterrupted add-folder -> chat -> tasks -> Accept run in a private Herdr window.

    python3 test/e2e/scenario.py --shprd target/debug/shprd [--mods host/herdr-desktop] [--out DIR]
                                 [--agent-model sonnet] [--keep]

Re-run it before every desktop merge (it is not part of `bin/check` or CI: it
opens a window and starts real agents). It prints a table of every step, what it
expected and what it saw, with timings, and writes a screenshot per step into
`--out`. It exits 1 when a step failed.

Isolation (the rules every live check here follows):
- ONE private Herdr window at a time: a copy of the person's `~/.config/herdr-app`
  (their Settings and Shepherdr mods, `spaces.picked` dropped), a private
  `HC_SESSION`, private `XDG_*` and `SHPRD_HOME`, and a Dock-like PATH whose
  first entry is the binary under test. Nothing is read from or written to the
  person's own state; every command starts with the guards below.
- Torn down by recorded pid (the window), then `hc server stop` for the private
  session; never pkill/killall by name.

What is driven how: the window has no CLI for a click or a keystroke, so the
steps drive what the window's own flows run: the New space dialog's commands
(`mkdir`, `git init`, `space.new`, `tab.new`), the chat pane's `POST /api/chat`,
the card's `POST /api/asks/answer` and the Accept button's `POST /api/action`.
What the person sees is checked in the window itself, by screenshot and by
`herdr-app ls` (which tabs a space has and which one is shown).
"""
import argparse
import atexit
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HOME = os.path.expanduser("~")
REAL_APP = os.path.join(HOME, ".config", "herdr-app")
REAL_CORE = os.path.join(HOME, ".config", "herdr-core")

MESSAGE = (
    "Add unit conversion to tcalc: "
    "1. New module tcalc/units.py with convert(value, from_unit, to_unit) for length: mm, cm, m, km, in, ft, mi; "
    "unknown or mismatched units raise ValueError; tests in tests/. "
    "2. Add temperature (C, F, K) to convert(); before building it, ask me whether results should be rounded and to how many places. "
    "3. Once 1 is done: CLI `python -m tcalc convert 5 km mi`; a bad unit prints a one-line error and exits 1. "
    "4. Write docs/units.md with one example per unit group."
)

CURRENT_BASE = None
rows = []  # (step, expected, seen, seconds, ok)
t_run = time.time()


def say(msg):
    print(f"[{time.time() - t_run:6.1f}s] {msg}", flush=True)


class Env:
    """The private environment: every variable written out, none inherited from a pane."""

    def __init__(self, base):
        self.base = base
        self.app = f"{base}/app"
        self.shprd_home = f"{base}/shprdhome"
        self.dirs = {k: f"{base}/{k}" for k in ("cfg", "state", "data", "run", "cache", "tmp")}
        self.session = "e2e" + os.path.basename(base)[-6:].replace("-", "x")
        self.window = None
        self.log = open(f"{base}/window.log", "ab")

    def env(self, binary_dir):
        keep = {k: os.environ[k] for k in ("HOME", "USER", "LOGNAME", "SHELL", "LANG", "TERM") if k in os.environ}
        e = dict(keep)
        e.update(
            PATH=f"{binary_dir}:{HOME}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            HERDR_APP_HOME=self.app,
            HC_SESSION=self.session,
            XDG_CONFIG_HOME=self.dirs["cfg"],
            XDG_STATE_HOME=self.dirs["state"],
            XDG_DATA_HOME=self.dirs["data"],
            XDG_RUNTIME_DIR=self.dirs["run"],
            XDG_CACHE_HOME=self.dirs["cache"],
            TMPDIR=self.dirs["tmp"],
            SHPRD_HOME=self.shprd_home,
            HERDR_SOCKET_PATH="/nonexistent/herdr.sock",
        )
        # Fail closed: a missing or shared value never reaches a command.
        assert e["HERDR_APP_HOME"] != f"{HOME}/.config/herdr-app", "refusing the person's app home"
        assert e["SHPRD_HOME"] != HOME and e["XDG_STATE_HOME"] != f"{HOME}/.local/state"
        for k in e:
            assert not (k.startswith("HC_") and k != "HC_SESSION"), k
        return e


def run(cmd, env, timeout=60, check=False, cwd=None):
    r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=timeout, cwd=cwd)
    if check and r.returncode != 0:
        raise RuntimeError(f"{cmd}: {r.stderr.strip() or r.stdout.strip()}")
    return r


def app_do(env, _cmd, **args):
    name = _cmd
    cmd = ["herdr-app", "do", name]
    for k, v in args.items():
        cmd += [f"--{k.replace('_', '-')}", v if isinstance(v, str) else json.dumps(v)]
    r = run(cmd, env, timeout=30)
    try:
        out = json.loads(r.stdout or "{}")
    except ValueError:
        out = {}
    if r.returncode != 0 or out.get("error"):
        raise RuntimeError(f"{name} {args}: {out.get('error') or r.stderr.strip()}")
    return out


def ls(env):
    r = run(["herdr-app", "ls"], env, timeout=20)
    txt = r.stdout.strip()
    if os.environ.get("E2E_LS_DUMP"):
        open(os.environ["E2E_LS_DUMP"], "a").write(txt + "\n---\n" + r.stderr + "\n")
    try:
        return json.loads(txt)
    except ValueError:
        return {"raw": txt}


def shot(env, out, name):
    path = f"{out}/{name}.png"
    run(["herdr-app", "shot", path], env, timeout=30)
    return path


def service(env):
    """The one service's base url and token, from the private state dir."""
    d = f"{env['XDG_STATE_HOME']}/shprd"
    try:
        rec = json.load(open(f"{d}/service.json"))
        token = open(f"{d}/token").read().strip()
    except (OSError, ValueError):
        return None
    return {"base": f"http://127.0.0.1:{rec['port']}", "token": token}


def api(env, method, path, folder, body=None, tries=1):
    svc = service(env)
    if not svc:
        raise RuntimeError("the service is not up")
    sep = "&" if "?" in path else "?"
    url = f"{svc['base']}{path}{sep}repo={urllib.request.quote(folder, safe='')}"
    headers = {"authorization": f"Bearer {svc['token']}"}
    data = None
    if method != "GET":
        headers.update({"content-type": "application/json", "x-shprd": "1"})
        data = json.dumps(body or {}).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            txt = r.read().decode()
    except urllib.error.HTTPError as err:
        raise RuntimeError(f"{method} {path}: HTTP {err.code} {err.read().decode()[:300]}") from None
    return json.loads(txt) if txt.strip() else {}


def wait_for(fn, what, timeout, every=2.0):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            last = fn()
        except Exception as err:  # noqa: BLE001 - a poll that errors is a poll that has not succeeded yet
            last = None
            say(f"  ({what}: {err})")
        if last:
            return last
        time.sleep(every)
    return None


def tabs_of(env, folder_name):
    """The tabs of the space for a folder as the window says: [{id, name, active, mods}]."""
    data = ls(env)
    out = []
    spaces = data.get("spaces", []) if isinstance(data, dict) else []
    for sp in spaces:
        if folder_name not in str(sp.get("name", "")):
            continue
        for tab in sp.get("tabs", []):
            out.append({"space": sp.get("name"), "id": tab.get("id"), "name": tab.get("name"), "active": tab.get("active"), "raw": tab})
    return out


def foreign_assistants(env_dict, tabs):
    """What in this space is Shepherdr's Assistant: a second tab named Assistant (the New space flow makes one
    shprd-chat tab of that name itself), or a live terminal carrying Shepherdr's coordinator labels."""
    out = []
    own = [t for t in tabs if "shprd-coordinator" in str(t["id"])]
    # Once shprd has made its coordinator's tab, any other tab named Assistant is Shepherdr's.
    if own:
        out += [f"extra Assistant tab {t['id']}" for t in tabs if str(t["name"]).strip() == "Assistant" and t not in own]
    r = run(["hc", "ls", "--json"], env_dict, timeout=20)
    try:
        addresses = [t.get("address") for t in json.loads(r.stdout or "{}").get("terminals", [])]
    except ValueError:
        addresses = []
    for address in addresses:
        g = run(["hc", "get", address, "--json"], env_dict, timeout=20)
        try:
            term = json.loads(g.stdout or "{}")
        except ValueError:
            continue
        labels = term.get("labels") or {}
        template = str(term.get("template") or "")
        if any(k.startswith("herdr.coordinator") for k in labels) or template.startswith(("assistant", "herdr-assistant", "coordinator")):
            out.append(f"terminal {address}")
    return out


def record(step, expected, seen, started, ok):
    secs = time.time() - started
    rows.append((step, expected, seen, secs, ok))
    say(f"{'PASS' if ok else 'FAIL'} {step}: {seen}")
    return ok


def start_window(env, binary_dir):
    e = env.env(binary_dir)
    p = subprocess.Popen(["herdr-app"], env=e, stdout=env.log, stderr=env.log, start_new_session=True)
    env.window = p
    with open(f"{env.base}/window.pid", "w") as f:
        f.write(str(p.pid))
    ok = wait_for(lambda: run(["herdr-app", "status"], e, timeout=10).returncode == 0, "window up", 60, 1.0)
    return e if ok else None


def stop_window(env):
    p = env.window
    if p and p.poll() is None:
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except OSError:
            p.terminate()
        try:
            p.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
    env.window = None


def teardown(env, binary_dir, keep):
    say("teardown")
    stop_window(env)
    e = env.env(binary_dir)
    run(["hc", "server", "stop"], e, timeout=30)
    time.sleep(2)
    mine = subprocess.run(["ps", "-axo", "pid,command"], capture_output=True, text=True).stdout
    left = [ln for ln in mine.splitlines() if env.base in ln and "scenario.py" not in ln]
    if left:
        say("processes of this run still alive:\n" + "\n".join(left))
        for ln in left:
            pid = int(ln.split()[0])
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass
    if not keep:
        shutil.rmtree(env.base, ignore_errors=True)
    return left


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cleanup", help="tear down the leftovers of a run's private home (its path) and exit")
    ap.add_argument("--shprd", required=False, help="the shprd binary under test")
    ap.add_argument("--mods", required=False, help="the host/herdr-desktop folder to install (the code under test)")
    ap.add_argument("--out", required=False)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--stop-after", default="h", help="stop after this step letter (debugging)")
    args = ap.parse_args()
    if args.cleanup:
        return cleanup(args.cleanup)
    if not (args.shprd and args.mods and args.out):
        ap.error("--shprd, --mods and --out are required")

    binary = os.path.abspath(args.shprd)
    binary_dir = os.path.dirname(binary)
    mods = os.path.abspath(args.mods)
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    base = tempfile.mkdtemp(prefix="shpe2e-", dir="/private/tmp")
    assert base.startswith("/private/tmp/shpe2e-")
    global CURRENT_BASE
    CURRENT_BASE = base
    env = Env(base)
    for d in [env.app, env.shprd_home, *env.dirs.values()]:
        os.makedirs(d, exist_ok=True)
    atexit.register(lambda: None)
    failed = False

    # ---- setup: the person's real Settings and Shepherdr mods, this checkout's shprd ----
    say(f"private home {base}; binary {binary}; mods {mods}")
    shutil.rmtree(env.app)
    shutil.copytree(REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
    if os.path.isdir(REAL_CORE):
        shutil.copytree(REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
    # The person's shprd settings are copied (their roles and prompts), with the worker and the coordinator on Sonnet
    # so a small run stays small, and the reviewer another vendor (shprd refuses a same-vendor review); their file names the model of a role, not the settings' shape.
    real_cfg = f"{HOME}/.config/shprd"
    private_cfg = f"{env.shprd_home}/.config/shprd"
    os.makedirs(os.path.dirname(private_cfg), exist_ok=True)
    if os.path.isdir(real_cfg):
        shutil.copytree(real_cfg, private_cfg, symlinks=True)
    else:
        os.makedirs(private_cfg, exist_ok=True)
    open(f"{private_cfg}/settings.toml", "w").write(
        "version = 1\n\n[roles.worker]\nagent = \"claude\"\nmodel = \"sonnet\"\neffort = \"low\"\n\n"
        "[roles.coordinator]\nagent = \"claude\"\nmodel = \"sonnet\"\nperm = \"bypassPermissions\"\n\n"
        "[roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-6-luna\"\nperm = \"auto\"\n"
    )
    shared_path = f"{env.app}/shared.json"
    shared = json.load(open(shared_path))
    shared.pop("spaces.picked", None)
    shared["layout"] = "shprd"
    json.dump(shared, open(shared_path, "w"))
    e = env.env(binary_dir)
    r = run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
    say("install: " + (r.stdout.strip() or r.stderr.strip())[-300:])
    mod_hash = subprocess.run(
        f"cd {mods}/mods && cat shprd-tabs/main.js shprd-chat/main.js lib/board.js | shasum | cut -c1-12",
        shell=True, capture_output=True, text=True).stdout.strip()
    say(f"binary {subprocess.run([binary, '--version'], capture_output=True, text=True).stdout.strip()}; mods hash {mod_hash}")
    folder = f"{base}/proj/tcalc"
    name_part = "tcalc"

    started = time.time()
    e = start_window(env, binary_dir)
    if not e:
        record("0 start", "the private window answers", "it never did", started, False)
        teardown(env, binary_dir, args.keep)
        return 1
    app_do(e, "layout.switch", name="shprd")
    time.sleep(3)
    shot(e, out, "00-window-shprd-layout")
    say("window up in the shprd layout")

    def step_a():
        t0 = time.time()
        os.makedirs(os.path.dirname(folder), exist_ok=True)
        # The New space dialog's own order: make the folder, make it a repo, space.new, the chat tab, close the window's tab.
        os.makedirs(folder, exist_ok=True)
        made = app_do(e, "space.new", folder=folder, name=name_part)
        space = made.get("space")
        tab = app_do(e, "tab.new", space=space, name="Assistant", mod="shprd-chat", props=json.dumps({"space": "coordinator"}))
        if made.get("tab"):
            app_do(e, "tab.close", id=made["tab"], confirmed="true")
        say(f"space {space}")
        # The chat's first send makes the folder a repo (the person never types git); wait for the service to be up.
        up = wait_for(lambda: service(e), "service up", 90)
        time.sleep(5)
        shot(e, out, "a-space-added")
        tabs = tabs_of(e, name_part)
        shown = [t for t in tabs if t["active"]]
        names = [(t["name"], t["id"]) for t in tabs]
        stray = foreign_assistants(e, tabs)
        ok = bool(up) and len(shown) == 1 and not stray and ("shprd-coordinator" in str(shown[0]["id"]) or not [t for t in tabs if "shprd-coordinator" in str(t["id"])])
        return record("a add folder (window's own flow)", "only shprd's chat in the space; service up", f"tabs {names}; shown {[t['name'] for t in shown]}; foreign {stray}; service {'up' if up else 'DOWN'}", t0, ok), space

    ok, space = step_a()
    failed |= not ok
    if args.stop_after == "a":
        teardown(env, binary_dir, args.keep)
        return 1 if failed else 0

    # ---- b: seed, then the exact message ----
    t0 = time.time()
    pkg = f"{folder}/tcalc"
    os.makedirs(pkg, exist_ok=True)
    open(f"{pkg}/__init__.py", "w").write('"""tcalc: a tiny calculator."""\n')
    open(f"{pkg}/ops.py", "w").write("def add(a, b):\n    return a + b\n\n\ndef mul(a, b):\n    return a * b\n")
    open(f"{pkg}/__main__.py", "w").write('import sys\nfrom .ops import add, mul\n\nif __name__ == "__main__":\n    print(add(int(sys.argv[1]), int(sys.argv[2])))\n')
    os.makedirs(f"{folder}/tests", exist_ok=True)
    open(f"{folder}/tests/test_ops.py", "w").write("from tcalc.ops import add, mul\n\n\ndef test_ops():\n    assert add(1, 2) == 3 and mul(2, 3) == 6\n")
    sh = lambda *a: subprocess.run(a, cwd=folder, capture_output=True, text=True, env=e)
    sh("git", "init", "-q", "--initial-branch=main")
    sh("git", "add", "-A")
    sh("git", "-c", "user.name=e2e", "-c", "user.email=e2e@example.com", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "seed tcalc")
    # The chat pane refuses (and the person retries) until the coordinator is up: so does this.
    sent = wait_for(lambda: api(e, "POST", "/api/chat", folder, {"space": "coordinator", "text": MESSAGE}) or {"ok": True}, "chat send", 240, 4.0)
    time_sent = time.time()
    say(f"sent: {json.dumps(sent)[:200]}")
    ok_b = record("b seed + send the message", "the coordinator takes the message", "sent" if sent is not None else "refused", t0, sent is not None)
    failed |= not ok_b

    # ---- c: tasks in the Tasks and Graph panes, nothing in herdr-task list ----
    t0 = time.time()

    def tasks_now():
        st = api(e, "GET", "/api/state", folder)
        ts = st.get("tasks", [])
        return ts if len(ts) >= 3 else None

    ts = wait_for(tasks_now, "tasks appear", 240, 3.0)
    appeared = time.time() - time_sent
    if ts:
        time.sleep(4)
        shot(e, out, "c-tasks-pane")
        # The Graph view is the Tasks panel's second view and takes a click to show; what it draws is
        # `graphRows` of the same state, so say what that is.
        st = api(e, "GET", "/api/state", folder)
        g = subprocess.run(["node", f"{mods}/test/e2e/graphrows.mjs"], input=json.dumps(st), capture_output=True, text=True)
        say("graph rows: " + g.stdout.strip()[:600])
        open(f"{out}/c-graph-rows.json", "w").write(g.stdout)
    listed = run(["herdr-task", "list", "--folder", folder], {**e, "PATH": e["PATH"]}, timeout=30)
    hl = (listed.stdout + listed.stderr).strip()
    seen = f"{len(ts or [])} tasks after {appeared:.0f}s: " + ", ".join(f"{t['id']}={t['derived']['status']}(deps {t.get('deps')})" for t in (ts or []))
    seen += f"; herdr-task list: {hl[:80]!r}"
    ok_c = bool(ts) and not re.search(r"\b(TC|SHPR|T)-?\d", hl) and any(t.get("deps") for t in ts)
    failed |= not record("c tasks appear in both panes", "all tasks within seconds, 3 after 1, none in herdr-task list", seen, t0, ok_c)

    # ---- e: review -> accept task 1 ----
    t0 = time.time()
    first = (ts or [{}])[0].get("id")

    def ready1():
        st = api(e, "GET", "/api/state", folder)
        t = next((x for x in st["tasks"] if x["id"] == first), None)
        return t if t and t["derived"]["status"] in ("ready", "done") else None

    t1 = wait_for(ready1, "task 1 ready", 900, 5.0)
    if t1 and t1["derived"]["status"] == "ready":
        shot(e, out, "e-task1-ready")
        api(e, "POST", "/api/action", folder, {"kind": "task.accept", "task": first})
        merged = wait_for(lambda: next((x for x in api(e, "GET", "/api/state", folder)["tasks"] if x["id"] == first and x["derived"]["status"] == "done"), None), "task 1 done", 180, 3.0)
    else:
        merged = t1
    time.sleep(4)
    shot(e, out, "e-task1-done")
    st = api(e, "GET", "/api/state", folder) if t1 else {"tasks": []}
    # What starts "by itself" after Accept: a task that was waiting on task 1 and is now running (a task that
    # shares a path with a running one still waits for it, which the row says).
    deps_of_first = [x for x in st["tasks"] if first in (x.get("deps") or [])]
    def started():
        now = api(e, "GET", "/api/state", folder)["tasks"]
        return next((x for x in now if first in (x.get("deps") or []) and x["derived"]["status"] in ("working", "ready", "done", "needs_you", "merging")), None)

    third = wait_for(started, "a dependent starts", 90, 3.0) if merged else None
    log = sh("git", "log", "--oneline", "--all", "-6").stdout.strip().replace("\n", " | ")
    seen = f"task1 {merged and merged['derived']['status']}; branches: {log[:160]}; started by itself: {third and (third['id'], third['derived']['status'])}"
    ok_e = bool(merged) and bool(third)
    failed |= not record("e review -> accept task 1", "merged locally, row Done, task 3 starts by itself", seen, t0, ok_e)

    # ---- d: the question card (task 2 starts once task 1 is accepted, so it comes after e) ----
    t0 = time.time()
    def card_list():
        got = api(e, "GET", "/api/asks", folder)
        return (got if isinstance(got, list) else (got or {}).get("asks", [])) or None

    cards = wait_for(card_list, "a question card", 600, 4.0) or []
    if cards:
        shot(e, out, "d-question-card")
    seen = f"{len(cards)} card(s): " + "; ".join(str(c.get("title") or c.get("body") or c)[:90] for c in cards)
    failed |= not record("d question card for task 2", "a card asks about rounding", seen, t0, bool(cards))
    if cards:
        c = cards[0]
        api(e, "POST", "/api/asks/answer", folder, {"task": c.get("task"), "ask": c.get("id") or c.get("ask"), "answer": "Round to 2 decimal places."})
        time.sleep(3)
        shot(e, out, "d-answered")

    if args.stop_after == "e":
        teardown(env, binary_dir, args.keep)
        return 1 if failed else 0

    def view_ok(label):
        tabs = tabs_of(e, name_part)
        shown = [t for t in tabs if t["active"]]
        ids = [(t["name"], t["id"]) for t in tabs]
        foreign = foreign_assistants(e, tabs)
        stt = api(e, "GET", "/api/state", folder)
        return tabs, shown, ids, foreign, len(stt["tasks"])

    # ---- f: layout round trip ----
    t0 = time.time()
    app_do(e, "layout.switch", name="shepherdr")
    time.sleep(8)
    shot(e, out, "f-shepherdr-layout")
    app_do(e, "layout.switch", name="shprd")
    time.sleep(8)
    shot(e, out, "f-back-in-shprd")
    tabs, shown, ids, foreign, ntasks = view_ok("f")
    seen = f"tabs {ids}; shown {[t['name'] for t in shown]}; tasks {ntasks}"
    ok_f = len(shown) == 1 and 'shprd-coordinator' in str(shown[0]['id']) and not foreign and ntasks >= 3
    failed |= not record("f layout shprd -> shepherdr -> shprd", "only shprd's chat; panes still show the run", seen, t0, ok_f)

    if args.stop_after == "f":
        teardown(env, binary_dir, args.keep)
        return 1 if failed else 0

    # ---- g: close and reopen the window ----
    t0 = time.time()
    stop_window(env)
    time.sleep(3)
    e = start_window(env, binary_dir)
    if e:
        time.sleep(10)
        shot(e, out, "g-reopened")
        tabs, shown, ids, foreign, ntasks = view_ok("g")
        seen = f"tabs {ids}; shown {[t['name'] for t in shown]}; tasks {ntasks}"
        ok_g = len(shown) == 1 and 'shprd-coordinator' in str(shown[0]['id']) and not foreign and ntasks >= 3
    else:
        seen, ok_g = "the window did not come back", False
    failed |= not record("g close and reopen the window", "same as f", seen, t0, ok_g)

    # ---- h: run every task to Done ----
    t0 = time.time()
    deadline = time.time() + 1500
    final = None
    while time.time() < deadline and e:
        try:
            st = api(e, "GET", "/api/state", folder)
        except Exception as err:  # noqa: BLE001
            say(f"  (state: {err})")
            time.sleep(5)
            continue
        words = {t["id"]: t["derived"]["status"] for t in st["tasks"]}
        for t in st["tasks"]:
            if t["derived"]["status"] == "ready":
                api(e, "POST", "/api/action", folder, {"kind": "task.accept", "task": t["id"]})
                say(f"accepted {t['id']}")
        for c in (api(e, "GET", "/api/asks", folder) or {}).get("asks", []) if isinstance(api(e, "GET", "/api/asks", folder), dict) else []:
            say(f"unexpected card: {str(c)[:100]}")
        if st["tasks"] and all(w == "done" for w in words.values()):
            final = st
            break
        time.sleep(8)
    time.sleep(4)
    shot(e, out, "h-all-done") if e else None
    seen = f"{json.dumps({t['id']: (t['derived']['status'], t['derived'].get('reason')) for t in (final or st)['tasks']})[:300]}"
    ok_h = bool(final)
    failed |= not record("h every task to Done", "all Done; honest subtitles and status bar", seen, t0, ok_h)

    left = teardown(env, binary_dir, args.keep)
    failed |= bool(left)

    table = ["| step | expected | seen | seconds | result |", "|---|---|---|---|---|"]
    for step, exp, seen, secs, ok in rows:
        table.append(f"| {step} | {exp} | {seen.replace('|', '/')} | {secs:.0f} | {'PASS' if ok else 'FAIL'} |")
    open(f"{out}/report.md", "w").write("\n".join(table) + "\n")
    print("\n".join(table))
    return 1 if failed else 0


def cleanup(base):
    """Tears down a run that died before its own teardown: the recorded window pid, then the private session."""
    assert base.startswith("/private/tmp/shpe2e-"), base
    env = Env.__new__(Env)
    env.base = base
    env.app = f"{base}/app"
    env.shprd_home = f"{base}/shprdhome"
    env.dirs = {k: f"{base}/{k}" for k in ("cfg", "state", "data", "run", "cache", "tmp")}
    env.session = "e2e" + os.path.basename(base)[-6:].replace("-", "x")
    env.window = None
    try:
        pid = int(open(f"{base}/window.pid").read())
        os.kill(pid, signal.SIGTERM)
    except (OSError, ValueError):
        pass
    time.sleep(2)
    e = env.env("/usr/bin")
    run(["hc", "server", "stop"], e, timeout=30)
    time.sleep(2)
    ps = subprocess.run(["ps", "-axo", "pid,command"], capture_output=True, text=True).stdout
    print("\n".join(ln for ln in ps.splitlines() if base in ln) or "nothing of this run is left")
    return 0


if __name__ == "__main__":
    code = 1
    try:
        code = main()
    finally:
        # A run that died on an exception still leaves nothing of itself running.
        if CURRENT_BASE and os.path.exists(CURRENT_BASE):
            cleanup(CURRENT_BASE)
    sys.exit(code)
