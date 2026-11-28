#!/usr/bin/env python3
"""A stand-in `claude` for a private live check (chat-ask): no model, no network.

It keeps a Claude-shaped transcript under $HOME/.claude/projects/<slug>/ and, when a file named
`.fake-ask` appears in its working directory, asks the multiple-choice question that file holds
(JSON: the AskUserQuestion input) the way Claude Code does: a numbered dialog, the status report
"blocked (question)" over OSC 7501, Down/Up/Enter to choose, "Type something." to type words. The
answer is written to `.fake-answer` and the tool result into the transcript. Anything else typed
is a chat line: appended to `.fake-lines` and to the transcript.
"""
import base64
import json
import os
import re
import select
import sys
import termios
import time
import tty

cwd = os.path.realpath(os.getcwd())
slug = re.sub(r"[^A-Za-z0-9]", "-", cwd)
folder = os.path.join(os.path.expanduser("~"), ".claude", "projects", slug)
os.makedirs(folder, exist_ok=True)
path = os.path.join(folder, "fake-session.jsonl")
out = sys.stdout


def stamp():
    return time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())


def append(kind, content):
    line = {"type": kind, "isSidechain": False, "timestamp": stamp(), "message": {"role": kind, "content": content}}
    with open(path, "a") as f:
        f.write(json.dumps(line) + "\n")


def osc(state, extra=""):
    out.write(f"\x1b]7501;state={state}{extra}\x1b\\")
    out.flush()


def key():
    ch = os.read(0, 1)
    if ch == b"\x1b":
        if select.select([0], [], [], 0.05)[0]:
            rest = os.read(0, 2)
            return {b"[A": "up", b"[B": "down"}.get(rest, "esc")
        return "esc"
    return {b"\r": "enter", b"\n": "enter", b"\x7f": "backspace"}.get(ch, ch.decode("utf-8", "ignore"))


def dialog(q, at, chosen_extra=None):
    out.write("\x1b[2J\x1b[H")
    out.write(f"\x1b[7m ☐ {q['header']} \x1b[0m\r\n\r\n{q['question']}\r\n\r\n")
    options = q["options"] + [{"label": "Type something.", "description": ""}]
    for n, option in enumerate(options, 1):
        out.write(f"{'❯' if n - 1 == at else ' '} {n}. {option['label']}\r\n")
        if option["description"]:
            out.write(f"     {option['description']}\r\n")
    out.write("\r\n──────\r\n  4. Chat about this\r\n\r\nEnter to select · ↑/↓ to navigate · Esc to cancel\r\n")
    if chosen_extra is not None:
        out.write(f"\r\nType your answer: {chosen_extra}")
    out.flush()


def ask(spec, number):
    tool_id = f"toolu_fake{number}"
    q = spec["questions"][0]
    append("assistant", [{"type": "tool_use", "id": tool_id, "name": "AskUserQuestion", "input": spec}])
    osc("blocked", ":kind=question:msg=" + base64.b64encode(q["question"].encode()).decode())
    fd = 0
    old = termios.tcgetattr(fd)
    answer = None
    try:
        tty.setraw(fd)
        at = 0
        typed = ""
        count = len(q["options"]) + 1
        while answer is None:
            dialog(q, at, typed if at == len(q["options"]) else None)
            k = key()
            if k == "down":
                at = min(at + 1, count - 1)
            elif k == "up":
                at = max(at - 1, 0)
            elif k == "enter":
                if at < len(q["options"]):
                    answer = q["options"][at]["label"]
                else:
                    # Like the real dialog: an empty Enter on the Type row declines.
                    answer = "(declined)" if typed == "" else "(typed) " + typed
            elif at == len(q["options"]) and k == "backspace":
                typed = typed[:-1]
            elif at == len(q["options"]) and len(k) == 1:
                typed += k
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
    out.write("\x1b[2J\x1b[H")
    out.write(f"ANSWER: {answer}\r\n")
    out.flush()
    open(".fake-answer", "w").write(answer)
    append("user", [{"type": "tool_result", "tool_use_id": tool_id, "content": f"User has answered your questions: {answer}"}])
    osc("working")
    time.sleep(0.5)
    append("assistant", [{"type": "text", "text": f"Thanks, you answered: {answer}"}])
    osc("idle")


append("user", "Reviews fail to start, why?")
append("assistant", [{"type": "text", "text": "The retry failed the same way. I need one decision from you."}])
osc("idle")
out.write("fake claude ready\r\n")
out.flush()
asked = 0
buffer = ""
def midturn(spec):
    """A message typed while the agent was mid-turn, the way Claude Code records it: a queue-operation
    pair, a queued_command attachment, then the reply. `blob` first writes one record longer than a
    read (a request dump), which is what once hid everything after it."""
    def raw(obj):
        with open(path, "a") as f:
            f.write(json.dumps(obj) + "\n")
    if spec.get("blob"):
        raw({"type": "api-request-blob", "blob": "x" * int(spec["blob"])})
    raw({"type": "queue-operation", "operation": "enqueue", "timestamp": stamp(), "content": spec["prompt"]})
    raw({"type": "queue-operation", "operation": "remove", "timestamp": stamp(), "content": spec["prompt"]})
    raw({"type": "attachment", "isSidechain": False, "timestamp": stamp(),
         "attachment": {"type": "queued_command", "prompt": spec["prompt"], "commandMode": "prompt", "origin": {"kind": "human"}}})
    raw({"type": "attachment", "isSidechain": False, "timestamp": stamp(),
         "attachment": {"type": "queued_command", "prompt": '<shprd-news task="demo" event="landed">demo is done: merged</shprd-news>'}})
    time.sleep(1)
    append("assistant", [{"type": "text", "text": spec["reply"]}])


while True:
    if os.path.exists(".fake-midturn"):
        spec = json.load(open(".fake-midturn"))
        os.remove(".fake-midturn")
        midturn(spec)
        continue
    if os.path.exists(".fake-ask"):
        spec = json.load(open(".fake-ask"))
        os.remove(".fake-ask")
        asked += 1
        ask(spec, asked)
        continue
    if select.select([0], [], [], 0.2)[0]:
        data = os.read(0, 4096).decode("utf-8", "ignore")
        buffer += data
        while "\n" in buffer or "\r" in buffer:
            line, _, buffer = re.split(r"(?:\r\n|\r|\n)", buffer, maxsplit=1)[0], None, re.split(r"(?:\r\n|\r|\n)", buffer, maxsplit=1)[1]
            if line.strip():
                open(".fake-lines", "a").write(line + "\n")
                append("user", line)
                out.write(f"YOU: {line}\r\n")
                out.flush()
