#!/usr/bin/env python3
"""SHPR-81: press-to-pane timing and a rollback, in one private Herdr window, with no agent.

    python3 test/e2e/press.py --shprd target/debug/shprd [--mods host/herdr-desktop] --out DIR [--keep] [--stop-after S]

The companion of `scenario.py` (SHPR-79/80, the whole add-folder -> chat -> Accept run with
real agents): it uses that script's private-window rules and helpers, and a fixture repository
built with shprd's own CLI (`fixture.py`) so every state a press needs is there at once.

What it measures: a press is `herdr-app do shprd-tasks.press:<verb>:<task>`, which runs the
same `verb` a row's pill runs (and so the one optimistic layer, `lib/optimistic.js`). Each pane
logs, on its own clock, the first time it drew the press's effect (the `e2e.probe` file, see
`probe()` in the layer); the op's id carries the press time (base 36), so
press-to-pane = pane's line - press time, per pane, in ms. The pressing is not a click: the
window has no pointer driver, so the time from a pointer to the handler is not in it.

Steps: a graph (every state word right-aligned) -> Accept T1 with the list shown (T2 starts) ->
Accept T4 with the Graph shown -> Accept T3 (a real merge conflict: Merging at once, then the
true state and one line) -> Stop T5 (the service refuses: back at once, one line).
Isolation is `scenario.py`'s: a copy of the real app home, private HC/XDG/SHPRD dirs, a
Dock-like PATH, one window, torn down by recorded pid.
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
from fixture import seed  # noqa: E402

FOLDER_NAME = "shop"


def op_time(op_id):
    """The press time (ms) the layer put in an op's id (base 36 before the dash)."""
    return int(op_id.split("-")[0], 36)


def probe_lines(path):
    try:
        return [json.loads(line) for line in open(path).read().splitlines() if line.strip()]
    except (OSError, ValueError):
        return []


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shprd", required=True)
    ap.add_argument("--mods", default=os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..")))
    ap.add_argument("--out", required=True)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--stop-after", default="z")
    args = ap.parse_args()
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
    probe = f"{base}/probe.log"
    summary = {"presses": []}

    shutil.rmtree(env.app)
    shutil.copytree(sc.REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
    if os.path.isdir(sc.REAL_CORE):
        shutil.copytree(sc.REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
    private_cfg = f"{env.shprd_home}/.config/shprd"
    os.makedirs(private_cfg, exist_ok=True)
    open(f"{private_cfg}/settings.toml", "w").write(
        "version = 1\n\n[roles.worker]\nagent = \"claude\"\nmodel = \"sonnet\"\neffort = \"low\"\n\n"
        "[roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-6-luna\"\nperm = \"auto\"\n"
    )
    shared_path = f"{env.app}/shared.json"
    shared = json.load(open(shared_path))
    shared["layout"] = "shprd"
    shared["spaces.picked"] = "herdr:sp0000dead0000000000000000000000000"
    shared["e2e.probe"] = probe
    json.dump(shared, open(shared_path, "w"))
    e = env.env(binary_dir)
    r = sc.run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
    sc.say("install: " + (r.stdout.strip() or r.stderr.strip())[-200:])
    folder = f"{base}/proj/{FOLDER_NAME}"
    seed(binary, folder, env.shprd_home, e)
    sc.say("fixture seeded: " + sc.run([binary, "--repo", folder, "--home", env.shprd_home, "--no-dagr", "status"], e).stdout.replace("\n", " | "))

    started = time.time()
    e = sc.start_window(env, binary_dir)
    if not e:
        sc.record("0 start", "the private window answers", "it never did", started, False)
        sc.teardown(env, binary_dir, args.keep)
        return 1
    try:
        sc.app_do(e, "layout.switch", name="shprd")
        time.sleep(3)
        made = sc.app_do(e, "space.new", folder=folder, name=FOLDER_NAME)
        space = made.get("space")
        say = sc.say
        say(f"space {space}")
        up = sc.wait_for(lambda: sc.service(e), "service up", 90)
        board = sc.wait_for(lambda: (lambda st: st if len(st.get("tasks", [])) >= 5 else None)(sc.api(e, "GET", "/api/state", folder)), "five tasks", 90, 2.0)
        sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)
        time.sleep(6)
        sc.shot(e, out, "00-list-before")
        sc.record("0 fixture in the window", "five tasks in known states", ", ".join(f"{t['id']}={t['derived']['status']}" for t in (board or {}).get("tasks", [])), started, bool(up and board))
        failed |= not (up and board)

        # ---- 1: the Graph, every state word at the right edge ----
        t0 = time.time()
        sc.app_do(e, "shprd-tasks.graph")
        time.sleep(3)
        sc.shot(e, out, "01-graph-aligned")
        sc.record("1 graph", "state words right-aligned (screenshot 01)", "taken", t0, True)
        sc.app_do(e, "shprd-tasks.tasks")
        time.sleep(1)
        if args.stop_after == "1":
            raise SystemExit(0)

        def press(step, kind, task, view, wait=1.5, shots=(0.0,)):
            """Presses, shoots at once, and reads what each pane logged."""
            t0 = time.time()
            before = len(probe_lines(probe))
            t_cmd = time.time() * 1000
            sc.app_do(e, f"shprd-tasks.press:{kind}:{task}")
            took_cmd = time.time() * 1000 - t_cmd
            shot_started = time.time() * 1000
            sc.shot(e, out, f"{step}-{view}-at-press")
            shot_ms = time.time() * 1000 - shot_started
            time.sleep(wait)
            sc.shot(e, out, f"{step}-{view}-after-{wait}s")
            lines = probe_lines(probe)[before:]
            ops = {ln["op"] for ln in lines}
            rows_ = {}
            for ln in lines:
                if ln["what"] == "error":
                    continue
                rows_.setdefault(ln["pane"], ln["at"] - op_time(ln["op"]))
            errors = {ln["pane"]: ln["at"] - op_time(ln["op"]) for ln in lines if ln["what"] == "error"}
            result = {"step": step, "kind": kind, "task": task, "view": view, "panes_ms": rows_, "error_panes_ms": errors,
                      "command_ms": round(took_cmd), "screenshot_ms": round(shot_ms), "ops": sorted(ops)}
            summary["presses"].append(result)
            say(f"  {step}: {json.dumps(result)}")
            return result, t0

        # ---- 2: Accept T1 with the list shown ----
        before_state = {t["id"]: t["derived"]["status"] for t in sc.api(e, "GET", "/api/state", folder)["tasks"]}
        r2, t0 = press("2-accept-T1", "task.accept", "T1", "list")
        panes = r2["panes_ms"]
        ok2 = "Tasks" in panes and all(v < 100 for v in panes.values()) and len(panes) >= 2
        sc.record("2 Accept T1 (list view)", "every pane that shows it drew Merging within 100 ms", json.dumps(panes) + f" (before: {before_state})", t0, ok2)
        failed |= not ok2

        # ---- 3: Accept T4 with the Graph shown ----
        sc.app_do(e, "shprd-tasks.graph")
        time.sleep(2)
        r3, t0 = press("3-accept-T4", "task.accept", "T4", "graph", wait=2.0)
        ok3 = "Tasks" in r3["panes_ms"] and all(v < 100 for v in r3["panes_ms"].values())
        sc.record("3 Accept T4 (graph view)", "the Graph drew Merging within 100 ms", json.dumps(r3["panes_ms"]), t0, ok3)
        failed |= not ok3
        # Let T1 and T4 land (the engine's next pass).
        landed = sc.wait_for(lambda: all(
            t["derived"]["status"] == "done" for t in sc.api(e, "GET", "/api/state", folder)["tasks"] if t["id"] in ("T1", "T4")) or None, "T1 and T4 done", 120, 3.0)
        sc.shot(e, out, "3b-after-landing-graph")
        sc.record("3b the engine lands them", "T1 and T4 Done", str(bool(landed)), t0, bool(landed))
        failed |= not landed
        if args.stop_after == "3":
            raise SystemExit(0)

        # ---- 4: Accept T3: a real merge conflict ----
        sc.app_do(e, "shprd-tasks.tasks")
        time.sleep(2)
        r4, t0 = press("4-accept-T3-conflict", "task.accept", "T3", "list", wait=0.6)
        merging = r4["panes_ms"]
        # The engine tries to land it and cannot: the row goes back to Ready with one line.
        rolled = sc.wait_for(lambda: next((ln for ln in probe_lines(probe) if ln["what"] == "error" and ln["op"] in r4["ops"]), None), "rollback seen by a pane", 120, 0.3)
        sc.shot(e, out, "4b-conflict-rolled-back-list")
        st = {t["id"]: t["derived"]["status"] for t in sc.api(e, "GET", "/api/state", folder)["tasks"]}
        errs = {ln["pane"]: ln["at"] - op_time(ln["op"]) for ln in probe_lines(probe) if ln["what"] == "error" and ln["op"] in r4["ops"]}
        ok4 = bool(rolled) and "Tasks" in merging and st.get("T3") in ("needs_you", "ready")
        sc.record("4 Accept T3 -> real merge conflict", "Merging at once, then the true state (Needs you) with the engine's one line", f"merging {json.dumps(merging)}; rollback seen {json.dumps(errs)} ms after the press; T3 is {st.get('T3')}", t0, ok4)
        failed |= not ok4

        # ---- 5: Stop T5: the service refuses (no agent to stop) ----
        r5, t0 = press("5-stop-T5-refused", "task.stop", "T5", "list", wait=2.5)
        st = {t["id"]: t["derived"]["status"] for t in sc.api(e, "GET", "/api/state", folder)["tasks"]}
        ok5 = "Tasks" in r5["panes_ms"] or bool(r5["error_panes_ms"])
        sc.record("5 Stop T5", "Stopping… at once; if the service refuses, back with one line", f"{json.dumps(r5['panes_ms'])} errors {json.dumps(r5['error_panes_ms'])}; T5 is {st.get('T5')}", t0, ok5)
        failed |= not ok5

        # ---- 6: Accept T5, which is not ready: the service refuses, the row is back at once with one line ----
        r6, t0 = press("6-accept-T5-refused", "task.accept", "T5", "list", wait=1.0)
        ok6 = "Tasks" in r6["panes_ms"] and "Tasks" in r6["error_panes_ms"]
        sc.record("6 Accept T5 (refused by the service)", "Merging at once, then back with one plain line", f"{json.dumps(r6['panes_ms'])}; rollback seen {json.dumps(r6['error_panes_ms'])} ms after the press", t0, ok6)
        failed |= not ok6
    finally:
        open(f"{out}/press-summary.json", "w").write(json.dumps(summary, indent=1))
        if os.path.exists(probe):
            shutil.copy(probe, f"{out}/probe.log")
        left = sc.teardown(env, binary_dir, args.keep)
        failed |= bool(left)
    table = ["| step | expected | seen | seconds | result |", "|---|---|---|---|---|"]
    for step, exp, seen, secs, ok in sc.rows:
        table.append(f"| {step} | {exp} | {seen.replace('|', '/')} | {secs:.0f} | {'PASS' if ok else 'FAIL'} |")
    open(f"{out}/press-report.md", "w").write("\n".join(table) + "\n")
    print("\n".join(table))
    return 1 if failed else 0


if __name__ == "__main__":
    code = 1
    try:
        code = main()
    finally:
        if sc.CURRENT_BASE and os.path.exists(sc.CURRENT_BASE):
            sc.cleanup(sc.CURRENT_BASE)
    sys.exit(code)
