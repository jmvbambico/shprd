"""A repository with a run in known states, built with shprd's own CLI and no agent (SHPR-81).

    from fixture import seed
    seed(binary, folder, shprd_home, env)

Tasks it makes (the recipe of `crates/shprd/tests/land.rs`, with `--record-only` spawns, so no program starts):

- T1  ready, its branch changes b.txt: Accept lands it.
- T2  waits for T1 (`--dep T1`): the task that "starts" when T1 is accepted.
- T3  ready, its branch changes a.txt the way T4's does (its declared scope is x.txt so both may be
      live): the second to land conflicts.
- T4  ready, its branch changes a.txt: lands first, so T3's Accept hits a real merge conflict.
- T5  working (spawned, never reported): Stop / Drop / Review have a row to act on.

Nothing here starts a program or makes a model call.
"""
import os
import subprocess


def git(folder, *args):
    r = subprocess.run(["git", *args], cwd=folder, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"git {args}: {r.stderr.strip()}")
    return r.stdout.strip()


def seed(binary, folder, home, env):
    """Makes `folder` a repository with the run and tasks above. `env` is the private environment."""
    assert home != os.path.expanduser("~"), "refusing the person's home"
    os.makedirs(folder, exist_ok=True)

    def shprd(*args, actor=None):
        cmd = [binary, "--repo", folder, "--home", home, "--no-dagr"]
        if actor:
            cmd += ["--as", actor]
        r = subprocess.run([*cmd, *args], env=env, capture_output=True, text=True, cwd=folder)
        if r.returncode != 0:
            raise RuntimeError(f"shprd {args}: {r.stderr.strip() or r.stdout.strip()}")
        return r.stdout

    git(folder, "init", "-q", "-b", "main")
    for k, v in (("user.name", "e2e"), ("user.email", "e2e@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")):
        git(folder, "config", k, v)
    for name in ("a.txt", "b.txt", "c.txt"):
        open(f"{folder}/{name}", "w").write("one\n")
    git(folder, "add", "-A")
    git(folder, "commit", "-q", "-m", "seed")
    os.makedirs(f"{folder}/.shprd", exist_ok=True)
    open(f"{folder}/.shprd/config.toml", "w").write('[gates]\nworker = ["true"]\nintegration = ["true"]\n\n[land]\nmerge = "ask"\n')
    shprd("run", "start", "--run-id", "r1", "--base", "main", "--integration", "shprd/r1/int")

    def branch(name, file, body):
        git(folder, "checkout", "-q", "-b", name, "main")
        open(f"{folder}/{file}", "w").write(body)
        git(folder, "add", "--", file)
        git(folder, "commit", "-q", "-m", f"{name}: {file}")
        git(folder, "checkout", "-q", "main")
        # The attempt's worktree is the real thing: a landing reads its head.
        git(folder, "worktree", "add", "-q", f"{folder}-wt/{name.split('/')[-1].upper()}", name)

    def add(task, title, scope, *deps):
        extra = [x for d in deps for x in ("--dep", d)]
        shprd("agent", "task", "add", "--id", task, "--title", title, "--scope", scope, "--acceptance", "it works", *extra)

    def spawn(task, br):
        shprd("agent", "spawn", "--record-only", "--task", task, "--attempt", "1", "--agent", "w1", "--worktree", f"{folder}-wt/{task}", "--branch", br)

    def passed(task, br, changed):
        spawn(task, br)
        shprd("agent", "report", "--check", "tests|true|0", "--task", task, "--attempt", "1", "--status", "completed", "--summary", "done", "--changed", changed, "--commit", git(folder, "rev-parse", br))
        # The reviewer's dispatch is a fact the multiplexer would record; written as the land tests do.
        path = f"{folder}/.shprd/events.jsonl"
        text = open(path).read()
        seq = len(text.splitlines()) + 1
        line = ('{"seq":%d,"at_ms":%d,"actor":{"type":"human"},"kind":"review_requested","task":"%s","attempt":1,"reviewer":"c1","degraded":false}'
                % (seq, 1_700_000_000_000 + seq, task))
        open(path, "w").write(text + line + "\n")
        shprd("agent", "gate", "pass", "--task", task, "--attempt", "1", "--gate", "lint")
        packet = f"{home}/.shprd/reviews/r1/{task}-a1"
        os.makedirs(packet, exist_ok=True)
        open(f"{packet}/findings.json", "w").write("[]")
        shprd("agent", "review-verdict", "--task", task, "--attempt", "1", actor="engine")

    branch("feature/t1", "b.txt", "t1\n")
    branch("feature/t3", "a.txt", "t3\n")
    branch("feature/t4", "a.txt", "t4\n")
    add("T1", "Add the b file change", "b.txt")
    add("T2", "Follow-up that waits for T1", "d.txt", "T1")
    add("T3", "Change the a file one way", "x.txt")
    add("T4", "Change the a file another way", "a.txt")
    add("T5", "A task still being worked on", "c.txt")
    passed("T1", "feature/t1", "b.txt")
    passed("T4", "feature/t4", "a.txt")
    passed("T3", "feature/t3", "x.txt")
    spawn("T5", "feature/t5")
