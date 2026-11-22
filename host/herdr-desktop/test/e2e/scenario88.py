#!/usr/bin/env python3
"""SHPR-88: a space made under the Shepherdr layout, switched to the shprd layout, gets shprd's coordinator.

    python3 test/e2e/scenario88.py --shprd target/debug/shprd --mods host/herdr-desktop --out DIR [--keep]

One private Herdr window (scenario.py's isolation: private HC/XDG/SHPRD dirs, a copy of the
person's app home, torn down by recorded pid). Steps:
  1. the window starts in the Shepherdr layout; a space is made on a git repo with history, and
     Shepherdr's own Assistant is started in it (what the Shepherdr layout does for a space);
  2. the window switches to the shprd layout: within the wait, the space has a
     `shprd-coordinator-<space>` tab that is the one shown, the Assistant tab is still there,
     its agent was neither killed nor typed into;
  3. the window switches back to the Shepherdr layout: the Assistant is still there.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import scenario as sc  # noqa: E402


def assist(e, tabs):
    """Shepherdr's Assistant in the space: its tab (by id) and any terminal carrying its labels."""
    return [t["id"] for t in tabs if str(t["name"]).strip() == "Assistant"] + [
        x for x in sc.foreign_assistants(e, tabs) if x.startswith("terminal")]


NAME = "shpr88"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shprd", required=True)
    ap.add_argument("--mods", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--wait", type=int, default=60, help="seconds the coordinator may take to appear")
    ap.add_argument("--earlier", action="store_true",
                    help="the person's order: the space was first used under the shprd layout (its service and coordinator ran), "
                         "then under Shepherdr (the coordinator was stopped, an Assistant tab made), then switched back")
    ap.add_argument("--real-repo", action="store_true",
                    help="the folder is a local clone of ~/projects/shprd with its .shprd (runs, handoff): git history and earlier runs")
    ap.add_argument("--probe", action="store_true", help="stop after making the Shepherdr space")
    args = ap.parse_args()
    global NAME
    NAME = "shprd" if args.real_repo else "shpr88"
    binary = os.path.abspath(args.shprd)
    binary_dir = os.path.dirname(binary)
    mods = os.path.abspath(args.mods)
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    base = tempfile.mkdtemp(prefix="shpe2e-", dir="/private/tmp")
    assert base.startswith("/private/tmp/shpe2e-")
    sc.CURRENT_BASE = base
    env = sc.Env(base)
    for d in [env.app, env.shprd_home, *env.dirs.values()]:
        os.makedirs(d, exist_ok=True)
    failed = False
    # The window finds `shprd` through the person's login shell (`zsh -lic`), whose profile puts
    # ~/.cargo/bin first: the binary under test would never run. A private ZDOTDIR puts it first.
    zdot = f"{base}/zdot"
    os.makedirs(zdot)
    open(f"{zdot}/.zprofile", "w").write(f'export PATH="{binary_dir}:$PATH"\n')
    plain_env = env.env
    env.env = lambda d: {**plain_env(d), "ZDOTDIR": zdot}
    try:
        shutil.rmtree(env.app)
        shutil.copytree(sc.REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
        if os.path.isdir(sc.REAL_CORE):
            shutil.copytree(sc.REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
        private_cfg = f"{env.shprd_home}/.config/shprd"
        os.makedirs(private_cfg, exist_ok=True)
        open(f"{private_cfg}/settings.toml", "w").write(
            'version = 1\n\n[roles.worker]\nagent = "claude"\nmodel = "sonnet"\neffort = "low"\n\n'
            '[roles.coordinator]\nagent = "claude"\nmodel = "sonnet"\nperm = "bypassPermissions"\n\n'
            '[roles.reviewer]\nagent = "codex"\nmodel = "gpt-6-luna"\nperm = "auto"\n'
        )
        shared_path = f"{env.app}/shared.json"
        shared = json.load(open(shared_path))
        shared["layout"] = "shepherdr"
        json.dump(shared, open(shared_path, "w"))
        e = env.env(binary_dir)
        r = sc.run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
        sc.say("install: " + (r.stdout.strip() or r.stderr.strip())[-200:])

        folder = f"{base}/proj/{'shprd' if args.real_repo else 'shpr88'}"
        os.makedirs(os.path.dirname(folder))
        if args.real_repo:
            real = os.path.expanduser("~/projects/shprd")
            subprocess.run(["git", "clone", "-q", "--local", real, folder], check=True)
            shutil.copytree(f"{real}/.shprd", f"{folder}/.shprd", dirs_exist_ok=True)
        else:
            os.makedirs(folder)
        sh = lambda *a: subprocess.run(a, cwd=folder, capture_output=True, text=True, env=e)
        if not args.real_repo:
            sh("git", "init", "-q", "--initial-branch=main")
        for i in range(0 if args.real_repo else 3):
            open(f"{folder}/f{i}.txt", "w").write(f"{i}\n")
            sh("git", "add", "-A")
            sh("git", "-c", "user.name=e2e", "-c", "user.email=e2e@example.com", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", f"history {i}")

        e = sc.start_window(env, binary_dir)
        if not e:
            sc.record("0 start", "the private window answers", "it never did", time.time(), False)
            return 1
        sc.app_do(e, "layout.switch", name="shprd" if args.earlier else "shepherdr")
        time.sleep(3)

        # ---- 1: a Shepherdr space with its Assistant ----
        t0 = time.time()
        made = sc.app_do(e, "space.new", folder=folder, name=NAME)
        space = made.get("space")
        if args.earlier:
            # The New space dialog's own flow under the shprd layout: the shprd chat tab, the starter tab closed.
            sc.app_do(e, "tab.new", space=space, name="Assistant", mod="shprd-chat", props=json.dumps({"space": "coordinator"}))
            if made.get("tab"):
                sc.app_do(e, "tab.close", id=made["tab"], confirmed="true")
            up = sc.wait_for(lambda: sc.service(e), "service up", 90)

            def own0():
                return [t for t in sc.tabs_of(e, NAME) if "shprd-coordinator" in str(t["id"])]

            sc.say(f"earlier: shprd coordinator tab {bool(sc.wait_for(own0, 'coordinator tab', 90, 2.0))}; service {bool(up)}")
            sc.app_do(e, "layout.switch", name="shepherdr")
            time.sleep(5)
            # Shepherdr's Assistant tab is made, then the shprd coordinator is stopped on purpose (the person's Assistant did it).
            sc.app_do(e, "tab.new", space=space, name="Assistant", mod="chat",
                      props=json.dumps({"role": "coordinator", "template": "assistant", "space": space, "folder": folder}))
            time.sleep(3)
            lst = sc.run(["hc", "ls", "--json"], e, timeout=20)
            for term in json.loads(lst.stdout or "{}").get("terminals", []):
                got = json.loads(sc.run(["hc", "get", term.get("address"), "--json"], e, timeout=20).stdout or "{}")
                if (got.get("labels") or {}).get("shprd.role") == "coordinator":
                    sc.say("stop the shprd coordinator " + sc.run(["hc", "kill", term["address"]], e, timeout=20).stdout.strip()[:100])
            time.sleep(4)
            for t in sc.tabs_of(e, NAME):
                if "shprd-coordinator" in str(t["id"]):
                    sc.app_do(e, "tab.close", id=t["id"], confirmed="true")
        time.sleep(12)
        sc.shot(e, out, "1-shepherdr-space")
        tabs = sc.tabs_of(e, NAME)
        assistants = assist(e, tabs)
        sc.say(f"space {space}; tabs {[(t['name'], t['id']) for t in tabs]}; assistants {assistants}")
        open(f"{out}/1-ls.json", "w").write(json.dumps(sc.ls(e), indent=1))
        sc.say(f"tabs {[(t['name'], t['id']) for t in tabs]}; assistants {assistants}")
        failed |= not sc.record("1 shepherdr space", "one space with Shepherdr's Assistant, no shprd coordinator",
                                f"tabs {[(t['name'], t['id']) for t in tabs]}; assistants {assistants}", t0,
                                bool(assistants) and not [t for t in tabs if "shprd-coordinator" in str(t["id"])])
        if args.probe or not assistants:
            return 1 if failed else 0
        # Shepherdr's Assistant agent, running in the space as the person's was: Shepherdr's own labels
        # (herdr.coordinator.space/tab, herdr.chat.slot), a program that only waits. Its screen is read at the end
        # to prove nothing was typed into it.
        bare = str(space).split("/")[-1]
        tab_id = [t["id"] for t in tabs if str(t["name"]).strip() == "Assistant"][0]
        r = sc.run(["hc", "run", "--bg", "--no-parent", "--name", "herdr-assistant-e2e", "--cwd", folder,
                    "--label", "herdr.role=coordinator", "--label", f"herdr.coordinator.space={bare}",
                    "--label", f"herdr.coordinator.tab={tab_id}", "--label", f"herdr.chat.slot=coord-{bare}-e2e",
                    "--", "sh", "-c", "echo SHEPHERDR-ASSISTANT; cat"], e, timeout=30)
        sc.say(f"foreign assistant: {(r.stdout + r.stderr).strip()[:200]}")
        time.sleep(3)
        tabs = sc.tabs_of(e, NAME)
        sc.shot(e, out, "1c-shepherdr-assistant-running")
        before = sorted(assist(e, tabs))
        sc.say(f"assistants before the switch: {before}")

        # ---- 2: switch to the shprd layout ----
        t0 = time.time()
        sc.app_do(e, "layout.switch", name="shprd")

        def own():
            ts = sc.tabs_of(e, NAME)
            mine = [t for t in ts if "shprd-coordinator" in str(t["id"])]
            return (ts, mine) if mine else None

        got = sc.wait_for(own, "shprd coordinator tab", args.wait, 2.0)
        time.sleep(6)
        sc.shot(e, out, "2-shprd-layout")
        tabs = sc.tabs_of(e, NAME)
        shown = [t for t in tabs if t["active"]]
        after = sorted(assist(e, tabs))
        ids = [(t["name"], t["id"]) for t in tabs]
        ok2 = bool(got) and len(shown) == 1 and "shprd-coordinator" in str(shown[0]["id"]) and after == before
        failed |= not sc.record("2 switch to shprd layout", "shprd-coordinator tab shown within the wait; Assistant untouched",
                                f"tabs {ids}; shown {[t['name'] for t in shown]}; assistants before {before} after {after}", t0, ok2)

        # ---- 2b: the coordinator ends while the layout is shown: the panes' next read starts another ----
        t0 = time.time()
        lst = sc.run(["hc", "ls", "--json"], e, timeout=20)
        old_addr = None
        for term in json.loads(lst.stdout or "{}").get("terminals", []):
            got = json.loads(sc.run(["hc", "get", term.get("address"), "--json"], e, timeout=20).stdout or "{}")
            if (got.get("labels") or {}).get("shprd.role") == "coordinator" and got.get("state") == "live":
                old_addr = term["address"]
                sc.run(["hc", "kill", old_addr], e, timeout=20)
        time.sleep(3)

        def replaced():
            for term in json.loads(sc.run(["hc", "ls", "--json"], e, timeout=20).stdout or "{}").get("terminals", []):
                if term.get("address") == old_addr or term.get("state") != "live":
                    continue
                got = json.loads(sc.run(["hc", "get", term["address"], "--json"], e, timeout=20).stdout or "{}")
                if (got.get("labels") or {}).get("shprd.role") == "coordinator":
                    return term["address"]
            return None

        new_addr = sc.wait_for(replaced, "a new coordinator", args.wait, 3.0)
        time.sleep(6)
        sc.shot(e, out, "2b-coordinator-restarted")
        tabs = sc.tabs_of(e, NAME)
        shown = [t for t in tabs if t["active"]]
        failed |= not sc.record("2b coordinator ended, shown again", "a new shprd coordinator within the wait, shown; Assistant untouched",
                                f"old {old_addr}, new {new_addr}; tabs {[(t['name'], t['id']) for t in tabs]}; shown {[t['name'] for t in shown]}", t0,
                                bool(old_addr and new_addr) and len(shown) == 1 and "shprd-coordinator" in str(shown[0]["id"]))

        # ---- 3: back to Shepherdr ----
        t0 = time.time()
        sc.app_do(e, "layout.switch", name="shepherdr")
        time.sleep(8)
        sc.shot(e, out, "3-back-in-shepherdr")
        tabs = sc.tabs_of(e, NAME)
        back = sorted(assist(e, tabs))
        screen = sc.run(["hc", "read", "herdr-assistant-e2e", "--lines", "20"], e, timeout=20).stdout
        typed = [ln for ln in screen.splitlines() if ln.strip() and "SHEPHERDR-ASSISTANT" not in ln]
        failed |= not sc.record("3 back to shepherdr", "the Assistant is still there, nothing typed into it",
                                f"assistants {back}; its screen beyond its first line: {typed}", t0, back == before and not typed)
    finally:
        left = sc.teardown(env, binary_dir, args.keep)
        sc.say(f"left over: {left}")
    print("\n".join(f"{'PASS' if ok else 'FAIL'}  {s}: {seen}" for s, _x, seen, _t, ok in sc.rows))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
