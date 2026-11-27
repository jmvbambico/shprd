#!/usr/bin/env python3
"""chat-ask: a question the coordinator's terminal waits on shows in the chat and is answered there.

    python3 test/e2e/scenario_ask.py --shprd target/debug/shprd --mods host/herdr-desktop --out DIR

ONE private Herdr window (live.py's isolation: private HC/XDG/SHPRD dirs, a copy of the app home, torn
down by recorded pid). The coordinator is `fake_claude.py` (no model): it writes a Claude-shaped
transcript and asks a numbered question on demand, reporting "blocked (question)" like the real one.
Steps: ask -> the chat shows the card (screenshot) -> a chat line is held, not a host error ->
a button answers the dialog with its keys -> the held line arrives -> a second question is answered
with words. Every result is a line in DIR/result.txt.
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

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = {
    "questions": [{
        "header": "Host",
        "question": "Reviews can't start. Can I set this repo's host to 'herdr-desktop' so reviews start again?",
        "multiSelect": False,
        "options": [
            {"label": "Yes, set it (Recommended)", "description": "Set host = herdr-desktop for this repo now."},
            {"label": "No", "description": "Leave it on auto; reviews stay blocked."},
        ],
    }]
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shprd", required=True)
    ap.add_argument("--mods", required=True)
    ap.add_argument("--out", required=True)
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
    results = []
    slug_dir = None

    def check(name, ok, seen=""):
        line = f"{'PASS' if ok else 'FAIL'} {name}: {seen}"
        results.append(line)
        sc.say(line)
        open(f"{out}/result.txt", "w").write("\n".join(results) + "\n")
        return ok

    fakebin = f"{base}/fakebin"
    os.makedirs(fakebin)
    shutil.copy(f"{HERE}/fake_claude.py", f"{fakebin}/claude")
    os.chmod(f"{fakebin}/claude", 0o755)
    zdot = f"{base}/zdot"
    os.makedirs(zdot)
    open(f"{zdot}/.zshrc", "w").write(f'export PATH="{fakebin}:{binary_dir}:$PATH"\n')
    plain_env = env.env

    def with_zdot(bdir):
        e = plain_env(bdir)
        e["ZDOTDIR"] = zdot
        return e

    env.env = with_zdot
    ok_all = False
    try:
        shutil.rmtree(env.app)
        shutil.copytree(sc.REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
        if os.path.isdir(sc.REAL_CORE):
            shutil.copytree(sc.REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
        cfg = f"{env.shprd_home}/.config/shprd"
        os.makedirs(cfg, exist_ok=True)
        open(f"{cfg}/settings.toml", "w").write(
            'version = 1\n\n[roles.worker]\nagent = "claude"\nmodel = "sonnet"\neffort = "low"\n\n'
            '[roles.coordinator]\nagent = "claude"\nmodel = "sonnet"\n'
        )
        shared_path = f"{env.app}/shared.json"
        shared = json.load(open(shared_path))
        shared["layout"] = "shprd"
        shared["spaces.picked"] = "herdr:sp0000dead0000000000000000000000000"
        json.dump(shared, open(shared_path, "w"))
        e = env.env(binary_dir)
        r = sc.run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
        sc.say("install: " + (r.stdout.strip() or r.stderr.strip())[-200:])
        folder = f"{base}/proj/askrepo"
        os.makedirs(folder, exist_ok=True)
        subprocess.run(["git", "init", "-q"], cwd=folder, check=True)
        open(f"{folder}/README.md", "w").write("askrepo\n")
        subprocess.run(["git", "add", "-A"], cwd=folder, check=True)
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"], cwd=folder, check=True)
        real = os.path.realpath(folder)
        import re
        slug_dir = os.path.join(sc.HOME, ".claude", "projects", re.sub(r"[^A-Za-z0-9]", "-", real))
        assert slug_dir.startswith(f"{sc.HOME}/.claude/projects/-private-tmp-shpe2e-")

        e = sc.start_window(env, binary_dir)
        if not e:
            check("0 window", False, "never answered")
            return
        sc.app_do(e, "layout.switch", name="shprd")
        time.sleep(3)
        made = sc.app_do(e, "space.new", folder=folder, name="askrepo")
        space = made.get("space")
        sc.app_do(e, "tab.new", space=space, name="Assistant", mod="shprd-chat", props=json.dumps({"space": "coordinator"}))
        if made.get("tab"):
            sc.app_do(e, "tab.close", id=made["tab"], confirmed="true")
        up = sc.wait_for(lambda: sc.service(e), "service up", 120)
        check("1 service up", bool(up))
        if not up:
            return

        def chat():
            return sc.api(e, "GET", "/api/chat?space=coordinator", folder)

        seen = sc.wait_for(lambda: chat().get("session") and chat(), "coordinator conversation", 180, 3.0)
        check("2 the coordinator's conversation is read", bool(seen), str((seen or {}).get("session")))
        if not seen:
            sc.shot(e, out, "no-conversation")
            return
        sc.shot(e, out, "1-before-question")

        # ---- ask ----
        json.dump(SPEC, open(f"{folder}/.fake-ask", "w"))
        waiting = sc.wait_for(lambda: chat().get("question"), "question on the page", 60, 2.0)
        check("3 the page carries the waiting question", bool(waiting), json.dumps(waiting)[:160] if waiting else "none")
        if not waiting:
            sc.shot(e, out, "no-question")
            return
        time.sleep(4)
        sc.shot(e, out, "2-card-in-chat")

        # ---- a chat line while it waits ----
        try:
            sc.api(e, "POST", "/api/chat", folder, {"space": "coordinator", "text": "also check the gate"})
            check("4 a send while it waits is refused, typed", False, "it was accepted")
        except RuntimeError as err:
            check("4 a send while it waits is refused, typed", "question_waiting" in str(err) and "herdr-core" not in str(err), str(err)[:200])
        sc.app_do(e, "shprd-chat.type:also check the gate")
        sc.app_do(e, "shprd-chat.enter")
        time.sleep(3)
        sc.shot(e, out, "3-send-held")
        lines = f"{folder}/.fake-lines"
        check("5 nothing was typed into the dialog", not os.path.exists(lines), "no line reached the terminal yet")

        # ---- answer by button 2 ("No") ----
        sc.app_do(e, "shprd-chat.choose:2")
        got = sc.wait_for(lambda: open(f"{folder}/.fake-answer").read() if os.path.exists(f"{folder}/.fake-answer") else None, "answer", 30, 1.0)
        check("6 pressing option 2 answers the dialog with option 2", got == "No", str(got))
        gone = sc.wait_for(lambda: chat().get("question") is None or None, "question closes", 30, 1.0)
        check("7 the card closes", bool(gone))
        delivered = sc.wait_for(lambda: open(lines).read() if os.path.exists(lines) else None, "held line", 40, 2.0)
        check("8 the held message is sent after the answer", bool(delivered) and "also check the gate" in delivered, str(delivered).strip())
        time.sleep(3)
        sc.shot(e, out, "4-after-answer")

        # ---- a second question, answered with words ----
        os.remove(f"{folder}/.fake-answer")
        json.dump(SPEC, open(f"{folder}/.fake-ask", "w"))
        sc.wait_for(lambda: chat().get("question"), "second question", 60, 2.0)
        time.sleep(3)
        sc.app_do(e, "shprd-chat.reply:neither, ask me later")
        got = sc.wait_for(lambda: open(f"{folder}/.fake-answer").read() if os.path.exists(f"{folder}/.fake-answer") else None, "typed answer", 30, 1.0)
        check("9 words answer through Type something", got == "(typed) neither, ask me later", str(got))
        ok_all = all(line.startswith("PASS") for line in results)
    finally:
        left = sc.teardown(env, binary_dir, False)
        if slug_dir and os.path.isdir(slug_dir):
            shutil.rmtree(slug_dir, ignore_errors=True)
        sc.say(f"torn down; processes left: {left}")
        open(f"{out}/result.txt", "a").write(f"torn down; processes left: {left}\n")
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
