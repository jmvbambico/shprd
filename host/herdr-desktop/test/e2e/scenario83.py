#!/usr/bin/env python3
"""SHPR-83: the person's own run, with real agents and real mouse clicks, in one private Herdr window.

    python3 test/e2e/scenario83.py --shprd target/debug/shprd --mods host/herdr-desktop --out DIR
                                   [--stop-on-fail] [--coordinator-model sonnet]

The prompt is the person's exact one (the history feature for tcalc). The folder is a private copy of
~/projects/scratchpad at 046b7a0. The person's own Settings (their worker, reviewer and roles) are
copied; only the coordinator's model is a flag (the person's is opus; a run's cost is a choice).

What is clicked for real (`realclick.py`: a CGEvent at the place the control is drawn, found by OCR of
`herdr-app shot`): each row's Review, the dialog's Accept, the question card's answer. What is driven by
the window's own commands: adding the folder (`space.new`, `tab.new`) and the chat's `POST /api/chat`
for the prompt (typing 600 characters into the chat pane has no pointer in it either way).
Isolation is `scenario.py`'s: private HC/XDG/SHPRD dirs, a copy of the app home, one window, torn down by
recorded pid. A click is refused unless the private window is the topmost one under the point.
"""
import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import scenario as sc  # noqa: E402
import realclick as rc  # noqa: E402

PROMPT = (
    "Add a history feature and more operations to tcalc: 1. tcalc/history.py: record(expr, result) appends one line to "
    "~/.tcalc_history (the path can be overridden with the TCALC_HISTORY env var), and last(n) returns the newest n entries. "
    "Tests in tests/. 2. Once 1 is done: a CLI command `python -m tcalc history [n]` that prints the last n entries (default 10), "
    "one per line. Every normal calculation gets recorded. 3. Add mul and div to tcalc/ops.py. Before building it, ask me what "
    "dividing by zero should do. 4. In a separate task running alongside 3: add mod and pow to tcalc/ops.py. "
    "5. Write docs/history.md with a short example session."
)
TOOLS = "/private/tmp/claude-501/s/tools"
SRC = os.path.expanduser("~/projects/scratchpad")
COMMIT = "046b7a0"


def words_in(path):
    return rc.words(path)[0]


def row_action(shot_path, task_id, label):
    """The centre of `label`'s pill on `task_id`'s row: the id is the first word of the row's second line."""
    ws, size = rc.words(shot_path)
    ids = []
    for w in ws:
        if w[0] < 0.59 * size[0] or w[4].strip(".,:") != task_id:
            continue
        same = [o for o in ws if o is not w and any(ch.isalnum() for ch in o[4]) and abs(o[1] - w[1]) < 14 and o[0] < w[0] and w[0] - o[0] < 400]
        if not same:
            ids.append(w)
    pills = [w for w in ws if w[4].strip(".,:") == label]
    for i in ids:
        near = [p for p in pills if i[1] - 100 <= p[1] <= i[1] + 8]
        if near:
            p = max(near, key=lambda q: q[0])
            return (p[0] + p[2] / 2, p[1] + p[3] / 2), size
    return None, size


def dialog_accept(shot_path):
    """The dialog's Accept: on the footer line that also holds Cancel (the chat may say "Accept" too)."""
    ws, size = rc.words(shot_path)
    cancel = [w for w in ws if w[4] == "Cancel"]
    if not cancel:
        return None, size
    c = max(cancel, key=lambda q: q[1])
    hits = [w for w in ws if w[4] == "Accept" and abs(w[1] - c[1]) < 25 and w[0] > c[0]]
    if not hits:
        return None, size
    w = hits[0]
    return (w[0] + w[2] / 2, w[1] + w[3] / 2), size


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shprd", required=True)
    ap.add_argument("--mods", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--coordinator-model", default="sonnet")
    ap.add_argument("--stop-on-fail", action="store_true")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--stop-after-ask", action="store_true")
    ap.add_argument("--reviewer", default="claude", choices=["claude", "person"])
    ap.add_argument("--minutes", type=int, default=45)
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
    rc.tools(TOOLS)
    failed = False
    say = sc.say

    shutil.rmtree(env.app)
    shutil.copytree(sc.REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
    shutil.copytree(sc.REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
    private_cfg = f"{env.shprd_home}/.config/shprd"
    os.makedirs(os.path.dirname(private_cfg), exist_ok=True)
    shutil.copytree(f"{sc.HOME}/.config/shprd", private_cfg, symlinks=True)
    settings = open(f"{private_cfg}/settings.toml").read()
    # The person's repo entry for shprd (a Sonnet worker) is not this folder's; the coordinator's model is the flag.
    settings = settings.replace('model = "opus[1m]"', f'model = "{args.coordinator_model}"')
    # The reviewer: another vendor than the worker (shprd refuses a same-vendor review). The person's own is pi/glm; their
    # codex quota can run out mid-run, so a private run reviews with Claude (a flag picks codex).
    import re
    if args.reviewer == "claude":
        settings = re.sub(r"(?ms)^\[roles\.reviewer\].*?(?=^\[|\Z)", '[roles.reviewer]\nagent = "claude"\nmodel = "sonnet"\nperm = "auto"\n\n', settings, count=1)
    open(f"{private_cfg}/settings.toml", "w").write(settings)
    shared_path = f"{env.app}/shared.json"
    shared = json.load(open(shared_path))
    shared["layout"] = "shprd"
    shared["spaces.picked"] = "herdr:sp0000dead0000000000000000000000000"
    json.dump(shared, open(shared_path, "w"))
    e = env.env(binary_dir)
    r = sc.run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
    say("install: " + (r.stdout.strip() or r.stderr.strip())[-200:])
    folder = f"{base}/proj/scratchpad"
    os.makedirs(os.path.dirname(folder), exist_ok=True)
    subprocess.run(["git", "clone", "-q", SRC, folder], check=True, env=e)
    subprocess.run(["git", "checkout", "-q", "-B", "main", COMMIT], cwd=folder, check=True, env=e)
    subprocess.run(["git", "remote", "remove", "origin"], cwd=folder, check=True, env=e)
    say(f"binary {subprocess.run([binary, '--version'], capture_output=True, text=True).stdout.strip()}; folder {folder} at {COMMIT}")

    t0 = time.time()
    e = sc.start_window(env, binary_dir)
    if not e:
        sc.record("0 start", "the private window answers", "it never did", t0, False)
        sc.teardown(env, binary_dir, args.keep)
        return 1
    pids = rc.window_pid(env.window.pid)

    def click(shot_path, px, size, what):
        ok = rc.click_px(pids, px, size, log=say)
        say(f"  real click on {what}: {'sent' if ok else 'REFUSED'}")
        return ok

    def shoot(name):
        return sc.shot(e, out, name)

    def ensure_tasks_pane():
        """The Tasks pane can be hidden (the real window's layout state is copied): the top-right Tasks button shows it."""
        for _ in range(2):
            p = shoot("pane-check")
            ws, size = rc.words(p)
            if [w for w in ws if w[0] > 0.59 * size[0] and w[1] < 420 and (w[4].startswith("Filter") or w[4].endswith("asks") or w[4] in ("Terminal", "Graph"))]:
                return True
            btn = [w for w in ws if w[1] < 70 and w[0] > 0.8 * size[0] and w[4] == "Tasks"]
            if btn:
                click(p, (btn[0][0] + btn[0][2] / 2, btn[0][1] + btn[0][3] / 2), size, "the Tasks button (the pane was hidden)")
                time.sleep(2)
        return False

    def state():
        return sc.api(e, "GET", "/api/state", folder)

    def by_id(st):
        return {t["id"]: t for t in st.get("tasks", [])}

    try:
        sc.app_do(e, "layout.switch", name="shprd")
        time.sleep(3)
        made = sc.app_do(e, "space.new", folder=folder, name="scratchpad")
        space = made.get("space")
        sc.app_do(e, "tab.new", space=space, name="Assistant", mod="shprd-chat", props=json.dumps({"space": "coordinator"}))
        if made.get("tab"):
            sc.app_do(e, "tab.close", id=made["tab"], confirmed="true")
        up = sc.wait_for(lambda: sc.service(e), "service up", 90)
        time.sleep(5)
        shoot("00-space-added")
        sc.record("0 add the folder", "space and chat up, service up", f"space {space}; service {'up' if up else 'DOWN'}", t0, bool(up))

        t0 = time.time()
        sent = sc.wait_for(lambda: sc.api(e, "POST", "/api/chat", folder, {"space": "coordinator", "text": PROMPT}) or {"ok": True}, "chat send", 300, 4.0)
        sc.record("1 send the person's prompt", "the coordinator takes it", "sent" if sent is not None else "refused", t0, sent is not None)
        t_sent = time.time()
        got = sc.wait_for(lambda: (lambda st: st if len(st.get("tasks", [])) >= 5 else None)(state()), "5 tasks", 420, 4.0)
        sc.record("2 five tasks appear", "T1..T5 within minutes; T2 after T1", ", ".join(f"{t['id']}={t['derived']['status']}(deps {t.get('deps')})" for t in (got or {}).get("tasks", [])), t_sent, bool(got))
        failed |= not got
        if not got:
            shoot("02-no-tasks")
            return 1
        sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)

        ensure_tasks_pane()
        accepted, answered, clicked_review, tries, review_tries, retried = set(), set(), {}, {}, {}, {}
        deadline = time.time() + args.minutes * 60
        last = {}
        while time.time() < deadline:
            st = state()
            ts = by_id(st)
            words = {i: (t["derived"]["status"], t["status"]) for i, t in ts.items()}
            if words != last:
                say(f"  board: {json.dumps(words)}")
                last = words
            if ts and all(t["derived"]["status"] == "done" for t in ts.values()):
                break
            # ---- the question card: answered by a real click on its option ----
            asks = sc.api(e, "GET", "/api/asks", folder)
            asks = asks if isinstance(asks, list) else (asks or {}).get("asks", [])
            for a in asks:
                key = (a.get("task"), a.get("id") or a.get("ask"))
                if key in answered:
                    continue
                t1 = time.time()
                options = a.get("options") or []
                pick = next((o for o in options if "raise" in o.lower()), options[0] if options else None)
                first = (pick or "").split()[0] if pick else ""
                seen, ok, opened = "task row not found on screen", False, False
                for attempt in range(4):
                    # The list moves as tasks change state: shoot, find the row and click in one go.
                    p = shoot(f"ask-{a.get('task')}-list")
                    ws, size = rc.words(p)
                    right0 = [w for w in ws if w[0] > 0.59 * size[0]]
                    if [w for w in right0 if w[4].lower().startswith("answ") or w[4].strip(".,:") == first]:
                        opened = True  # the thread is already open on the card
                        break
                    idw = [w for w in ws if w[0] > 0.59 * size[0] and w[4].strip(".,:") == a.get("task") and not [o for o in ws if o is not w and any(ch.isalnum() for ch in o[4]) and abs(o[1] - w[1]) < 14 and o[0] < w[0] and w[0] - o[0] < 400]]
                    if not idw:
                        ensure_tasks_pane()
                        sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)
                        time.sleep(2)
                        continue
                    click(p, (idw[0][0] + 300, idw[0][1] - 40), size, f"the {a.get('task')} row")
                    # The thread opens at once; its question card draws a moment later.
                    for _ in range(6):
                        time.sleep(2)
                        p = shoot(f"ask-{a.get('task')}-card")
                        ws2, size2 = rc.words(p)
                        right = [w for w in ws2 if w[0] > 0.59 * size2[0]]
                        if [w for w in right if w[4].lower().startswith("answ") or w[4].strip(".,:") == first]:
                            opened = True
                            break
                    if opened:
                        break
                    if [w for w in right if w[1] < 400 and w[4].strip(".,:") == a.get("task")]:
                        continue  # in the thread already: wait for the card again on the next pass
                    sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)
                    time.sleep(1.5)
                if opened:
                    say(f"  ask options: {options}; picking {pick!r}")
                    px = None
                    for tok in (pick or "").replace("(", " ").split()[:3]:
                        px = rc.find(p, tok, nth=-1, region=(int(0.59 * size[0]), 0, size[0], size[1]))
                        if px:
                            first = tok
                            break
                    seen = f"opened the row; '{first}' {'found' if px else 'NOT on screen'}"
                    if px:
                        _, size = rc.words(p)
                        click(p, px, size, f"answer '{pick}'")
                        def gone():
                            now = sc.api(e, "GET", "/api/asks", folder)
                            now = now if isinstance(now, list) else (now or {}).get("asks", [])
                            return not [x for x in now if (x.get("id") or x.get("ask")) == key[1]] or None
                        ok = bool(sc.wait_for(gone, "ask answered", 30, 2.0))
                        seen = f"clicked '{first}' at {px[0]:.0f},{px[1]:.0f}; card gone: {ok}"
                        shoot(f"ask-{a.get('task')}-answered")
                tries[key] = tries.get(key, 0) + 1
                if not ok and tries[key] < 3:
                    say(f"  (ask {a.get('task')}: {seen}; trying again)")
                    continue
                answered.add(key)
                sc.record(f"ask {a.get('task')} answer by click", f"the card's answer ('{pick}') is taken", seen, t1, ok)
                failed |= not ok
                if args.stop_after_ask:
                    return 0 if ok else 1
            # ---- a review that could not start: the row's Retry review button, by a real click ----
            for tid, t in sorted(ts.items()):
                d = t["derived"]
                if t["status"] == "reported" and d["status"] == "needs_you" and str(d.get("reason", "")).startswith("review kickoff not delivered") and retried.get(tid, 0) < 3:
                    retried[tid] = retried.get(tid, 0) + 1
                    ensure_tasks_pane()
                    sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)
                    time.sleep(2)
                    t1 = time.time()
                    p = shoot(f"{tid}-kickoff-card")
                    px, size = row_action(p, tid, "Retry")
                    if not px:
                        say(f"  ({tid}: no Retry pill found yet)")
                        continue
                    click(p, px, size, f"{tid} Retry review")
                    cleared = sc.wait_for(lambda: (lambda x: x if x and x["derived"]["status"] != "needs_you" else None)(by_id(state()).get(tid)), f"{tid} review restarted", 90, 3.0)
                    shoot(f"{tid}-after-retry")
                    sc.record(f"{tid} Retry review (real click)", "the card goes and the review runs again", f"{tid} is {by_id(state())[tid]['derived']['status']}", t1, bool(cleared))
                    failed |= not cleared
            # ---- Review and Accept, by real clicks (a task that passed review reads Ready, or Needs you with a card beside it) ----
            for tid, t in sorted(ts.items()):
                if not (t["derived"]["status"] == "ready" or (t["status"] == "passed" and t["derived"]["status"] == "needs_you")) or tid in accepted:
                    continue
                t1 = time.time()
                review_tries[tid] = review_tries.get(tid, 0) + 1
                if review_tries[tid] >= 3:
                    accepted.add(tid)
                for _ in range(3):
                    p = shoot(f"{tid}-ready-list")
                    px, size = row_action(p, tid, "Review")
                    if px:
                        break
                    # The pane may still show a task's thread (the card was answered there): back to the list.
                    # The pane may still show a task's thread (the card was answered there). The pane's own
                    # command shows the list (its "‹ Tasks" link is a click the OCR reads as "‹asks").
                    sc.run(["herdr-app", "do", "layout.select", "--stack", "tasks", "--child", "tasks-list"], e, timeout=20)
                    time.sleep(2)
                if not px:
                    ready_now = [i for i, x in by_id(state()).items() if x["derived"]["status"] == "ready" or (x["status"] == "passed" and x["derived"]["status"] == "needs_you")]
                    pills = [w for w in rc.words(p)[0] if w[0] > 0.59 * size[0] and w[4] == "Review"]
                    if len(ready_now) == 1 and len(pills) == 1:  # OCR lost the id word; the one pill is this task's
                        px = (pills[0][0] + pills[0][2] / 2, pills[0][1] + pills[0][3] / 2)
                if not px:
                    if review_tries[tid] >= 3:
                        sc.record(f"{tid} Review", "the row shows a Review pill", "no Review pill found on the row", t1, False)
                        failed = True
                    continue
                accepted.add(tid)
                click(p, px, size, f"{tid} Review")
                time.sleep(2.5)
                p = shoot(f"{tid}-dialog")
                apx, size = dialog_accept(p)
                if not apx:  # the click may only have focused the window: once more
                    click(p, px, size, f"{tid} Review (again)")
                    time.sleep(3)
                    p = shoot(f"{tid}-dialog")
                    apx, size = dialog_accept(p)
                if not apx:
                    sc.record(f"{tid} Review", "the Accept dialog opens", "no dialog Accept found", t1, False)
                    failed = True
                    continue
                click(p, apx, size, f"{tid} Accept")
                moved = sc.wait_for(lambda: (lambda x: x if x and x["derived"]["status"] in ("merging", "done") or (x and x["status"] == "integrated") else None)(by_id(state()).get(tid)), f"{tid} merging", 60, 2.0)
                shoot(f"{tid}-after-accept")
                seen = f"{tid} is {by_id(state())[tid]['derived']['status']} {time.time() - t1:.0f}s after the click"
                sc.record(f"{tid} Review, then Accept (real clicks)", "the row goes Merging then Done", seen, t1, bool(moved))
                if not moved:
                    failed = True
                    if args.stop_on_fail:
                        return 1
            time.sleep(6)
        final = by_id(state())
        shoot("zz-final")
        sc.record("3 every task Done", "all five Done", json.dumps({i: t["derived"]["status"] for i, t in final.items()}),
                  t_sent, all(t["derived"]["status"] == "done" for t in final.values()))
        failed |= not all(t["derived"]["status"] == "done" for t in final.values())

        # ---- what the person was told, from the log and the state ----
        logs = glob.glob(f"{env.shprd_home}/**/events.jsonl", recursive=True)
        text = open(logs[0]).read() if logs else ""
        events = [json.loads(ln) for ln in text.splitlines() if ln.strip()]
        shutil.copy(logs[0], f"{out}/events.jsonl") if logs else None
        blob = json.dumps(state())
        cmd_told = [ev["body"] for ev in events if ev.get("kind") == "escalated" and "shprd approve" in ev.get("body", "")]
        sc.record("4 no command in what the person sees", "no 'shprd approve' in the state, the cards or the escalations",
                  f"in /api/state: {'shprd approve' in blob}; escalations naming it: {len(cmd_told)}", time.time(), "shprd approve" not in blob and not cmd_told)
        false_esc = [ev["body"][:90] for ev in events if ev.get("kind") == "escalated" and ev.get("actor", {}).get("type") == "agent" and ("never asked" in ev["body"] or "on its own" in ev["body"])]
        sc.record("5 the coordinator's claims match the log", "no 'never asked' escalation for a task whose ask was answered",
                  f"{len(false_esc)} such: {false_esc}", time.time(), not false_esc)
        failed |= bool(cmd_told or false_esc)
    finally:
        left = sc.teardown(env, binary_dir, args.keep)
        failed |= bool(left)
    table = ["| step | expected | seen | seconds | result |", "|---|---|---|---|---|"]
    for step, exp, seen, secs, ok in sc.rows:
        table.append(f"| {step} | {exp} | {seen.replace('|', '/')} | {secs:.0f} | {'PASS' if ok else 'FAIL'} |")
    open(f"{out}/report.md", "w").write("\n".join(table) + "\n")
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
