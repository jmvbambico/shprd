#!/usr/bin/env python3
"""SHPR-84: ONE private Herdr window with a coordinator, held up while a person (or an agent) drives it.

    python3 test/e2e/live.py --shprd target/debug/shprd --mods host/herdr-desktop --out DIR [--program claude] [--model sonnet]

It builds the same private home as `scenario.py` (a copy of the real app home, private HC/XDG/SHPRD
dirs, one window, torn down by recorded pid), adds a folder as a space with shprd's chat tab, waits for
the coordinator, then writes `<base>/env.sh` (the private environment, to `source` before any
`herdr-app`/`hc` command) and `<base>/state.json`, and waits for `<base>/quit` (or --max
seconds), then tears everything down. The window has no key or pointer driver that leaves the person's foreground alone, so the composer's own
handlers are pressed by name: `herdr-app do shprd-chat.<command>` (see `command()` in the mod).
The coordinator runs with claude's default permission mode, not bypass: the strict case an attachment
must still be readable in.
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shprd", required=True)
    ap.add_argument("--mods", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--program", default="claude")
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--max", type=int, default=1500)
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
    # The window finds `shprd` through the person's login shell (`resolveShprd`), which would find
    # their installed one first. A private ZDOTDIR puts the binary under test ahead of it for this
    # window's shells only (the person's rc files are not read, and nothing of theirs is written).
    zdot = f"{base}/zdot"
    os.makedirs(zdot, exist_ok=True)
    open(f"{zdot}/.zshrc", "w").write(f'export PATH="{binary_dir}:$PATH"\n')
    plain_env = env.env

    def with_zdot(bdir):
        e = plain_env(bdir)
        e["ZDOTDIR"] = zdot
        return e

    env.env = with_zdot
    try:
        shutil.rmtree(env.app)
        shutil.copytree(sc.REAL_APP, env.app, ignore=shutil.ignore_patterns("herdr-app.sock", "*.sock"), symlinks=True)
        if os.path.isdir(sc.REAL_CORE):
            shutil.copytree(sc.REAL_CORE, f"{env.dirs['cfg']}/herdr-core", symlinks=True, ignore=shutil.ignore_patterns("*.sock"))
        cfg = f"{env.shprd_home}/.config/shprd"
        os.makedirs(cfg, exist_ok=True)
        open(f"{cfg}/settings.toml", "w").write(
            f'version = 1\n\n[roles.worker]\nagent = "claude"\nmodel = "sonnet"\neffort = "low"\n\n'
            f'[roles.coordinator]\nagent = "{args.program}"\nmodel = "{args.model}"\n'
        )
        shared_path = f"{env.app}/shared.json"
        shared = json.load(open(shared_path))
        shared["layout"] = "shprd"
        shared["spaces.picked"] = "herdr:sp0000dead0000000000000000000000000"
        json.dump(shared, open(shared_path, "w"))
        e = env.env(binary_dir)
        r = sc.run([binary, "install", "herdr-desktop", "--from", mods, "--app-home", env.app], e, timeout=60)
        sc.say("install: " + (r.stdout.strip() or r.stderr.strip())[-200:])
        folder = f"{base}/proj/tcalc"
        os.makedirs(folder, exist_ok=True)
        subprocess.run(["git", "init", "-q"], cwd=folder, check=True)
        open(f"{folder}/README.md", "w").write("tcalc\n")
        subprocess.run(["git", "add", "-A"], cwd=folder, check=True)
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"], cwd=folder, check=True)
        e = sc.start_window(env, binary_dir)
        assert e, "window did not start"
        sc.app_do(e, "layout.switch", name="shprd")
        time.sleep(3)
        made = sc.app_do(e, "space.new", folder=folder, name="tcalc")
        space = made.get("space")
        sc.app_do(e, "tab.new", space=space, name="Assistant", mod="shprd-chat", props=json.dumps({"space": "coordinator"}))
        if made.get("tab"):
            sc.app_do(e, "tab.close", id=made["tab"], confirmed="true")
        sc.wait_for(lambda: sc.service(e), "service up", 90)
        time.sleep(8)
        with open(f"{base}/env.sh", "w") as f:
            for k, v in e.items():
                f.write(f"export {k}='{v}'\n")
            f.write(f"export SHPR_PID={env.window.pid}\nexport SHPR_BASE='{base}'\nexport SHPR_FOLDER='{folder}'\n")
        json.dump({"pid": env.window.pid, "base": base, "folder": folder}, open(f"{base}/state.json", "w"))
        open(f"{out}/last-base", "w").write(base)
        sc.say(f"READY base={base} pid={env.window.pid}")
        end = time.time() + args.max
        while time.time() < end and not os.path.exists(f"{base}/quit"):
            time.sleep(2)
    finally:
        left = sc.teardown(env, binary_dir, False)
        sc.say(f"torn down; left: {left}")


if __name__ == "__main__":
    main()
