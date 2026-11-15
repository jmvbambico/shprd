#!/usr/bin/env python3
"""A stand-in for `hc connect`, speaking the framing M7ef recorded.

The real thing is herdr-core's own CLI; this fake lets a unit test drive the
client over a real pipe — same bytes, same ordering, same frames — without a
core, a session or a person's Herdr desktop. Every answer it gives is a
transcript from `_research/m7/M7ef-herdr-desktop-spikes.md`: the `hello` result,
the `events.subscribe` snapshot, the twelve event types and the error shapes.

Behaviour is chosen per test with a small script directory:

    <dir>/mode          first line: what to do
    <dir>/child-marker  written when the child starts, so a test can count spawns
    <dir>/log           one line per request the fake answered

Modes:

    normal    answer every call; fail nothing
    old       answer `hello` with an old version, then exit
    gen2      answer `hello` with generation 2, then exit
    die-once  answer calls until `terminal.list` arrives a second time, then
              exit without answering (the reconnect case)
    quiet     answer `hello` and `events.subscribe`, then answer nothing at all
              (the timeout case)
    missed    answer normally, then send a `missed` notice with a fresh set, as
              a subscriber that fell behind receives (M7ef §2.4)
    doc       answer normally, then send a `doc` event naming `herdr-app`, which
              carries a version and no value
"""

import base64
import json
import os
import struct
import sys

FRAME_DATA = 0
FRAME_CREDIT = 1
FRAME_CLOSE = 2
EVENT_CHANNEL = 4

HELLO_METHODS = [
    "hello", "server.status", "server.stop", "server.restart", "terminal.run",
    "templates.list", "terminal.list", "terminal.get", "terminal.kill",
    "terminal.rm", "terminal.keep", "terminal.label", "terminal.read",
    "terminal.attach", "terminal.send", "terminal.wait", "terminal.keys",
    "terminal.interrupt", "terminal.put", "machine.exec", "file.read",
    "file.write", "file.edit", "doc.get", "doc.set", "events.subscribe",
    "machine.add", "machine.list", "machine.connect", "machine.rename",
    "machine.rm", "rpc.discover",
]

SPACE = "spc3f2a"
TABS = [("tbroot", ""), ("tbprobe", "probe")]

# The layout document value, as M7ef §2.7 recorded it.
LAYOUT = {
    "spaces": [{
        "id": SPACE,
        "name": "shprd probe",
        "folder": "/tmp/shs/tmp",
        "tabs": [
            {"id": "tbroot", "name": "", "root": {"type": "row", "id": "tbroot", "children": [
                {"type": "block", "id": "p1", "content": {"terminal": "t1y"}, "size": {"weight": 1.0}}]}},
            {"id": "tbprobe", "name": "probe", "root": {"type": "row", "id": "tbprobe", "children": [
                {"type": "column", "id": "s1", "size": {"weight": 0.5}, "children": [
                    {"type": "block", "id": "p2", "content": {"mod": "shprd-probe"}},
                    {"type": "block", "id": "p3", "content": {"mod": "workspace-status"}}]},
                {"type": "block", "id": "p4", "content": {"terminal": "t20"}, "size": {"weight": 0.5}}]}},
        ],
    }]
}


def terminal(address, labels=None, state="live", status=None, turn=0):
    return {
        "address": address,
        "name": address,
        "labels": labels or {},
        "argv": ["/bin/sh"],
        "cwd": "/tmp",
        "state": state,
        "status": status,
        "records": {},
        "reported": status is not None,
        "turn": turn,
        "pid": 4242,
        "created_ms": 1794618600000,
        "cols": 80,
        "rows": 24,
    }


class Fake:
    def __init__(self):
        self.dir = os.environ.get("FAKE_HC_DIR", "")
        self.mode = self.read_mode()
        self.framed = False
        self.terminals = {
            "t1y": terminal("t1y", {"herdr.space": SPACE, "herdr.tab": "tbroot"}),
            "t20": terminal("t20", {"herdr.space": SPACE, "herdr.tab": "tbprobe"}),
        }
        self.doc_version = 3
        self.list_calls = 0

    def path(self, name):
        return os.path.join(self.dir, name)

    def read_mode(self):
        try:
            with open(self.path("mode")) as handle:
                return handle.read().strip() or "normal"
        except OSError:
            return "normal"

    def log(self, line):
        if not self.dir:
            return
        with open(self.path("log"), "a") as handle:
            handle.write(line + "\n")

    def mark_spawn(self):
        if not self.dir:
            return
        marker = self.path("child-marker")
        count = 0
        if os.path.exists(marker):
            with open(marker) as handle:
                count = int(handle.read().strip() or "0")
        with open(marker, "w") as handle:
            handle.write(str(count + 1))
        with open(self.path("life"), "a") as handle:
            handle.write(f"started {count + 1}\n")

    def mark_stop(self):
        if not self.dir:
            return
        with open(self.path("life"), "a") as handle:
            handle.write("stopped\n")

    def write_plain(self, message):
        sys.stdout.buffer.write(json.dumps(message).encode() + b"\n")
        sys.stdout.buffer.flush()
        self.framed = bool(
            message.get("result", {}).get("framed", self.framed)
            if isinstance(message.get("result"), dict) else self.framed
        )

    def write_frame(self, channel, kind, payload):
        if isinstance(payload, str):
            payload = payload.encode()
        head = struct.pack("<IBI", channel, kind, len(payload))
        sys.stdout.buffer.write(head + payload)
        sys.stdout.buffer.flush()

    def write_rpc(self, message):
        body = json.dumps(message)
        if self.framed:
            self.write_frame(0, FRAME_DATA, body)
        else:
            self.write_plain(message)
            return

    def write_event(self, method, params):
        self.write_frame(EVENT_CHANNEL, FRAME_DATA, json.dumps(
            {"jsonrpc": "2.0", "method": method, "params": params}) + "\n")

    def read_message(self):
        if not self.framed:
            line = sys.stdin.buffer.readline()
            if not line:
                return None
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                return None
        head = self.read_exactly(9)
        if head is None:
            return None
        channel, kind, length = struct.unpack("<IBI", head)
        body = self.read_exactly(length)
        if body is None:
            return None
        if channel == 0 and kind == FRAME_DATA:
            try:
                return json.loads(body)
            except json.JSONDecodeError:
                return None
        if kind == FRAME_DATA:
            # Credit what we were sent, as the real core needs (M7ef §2.6).
            self.write_frame(channel, FRAME_CREDIT, struct.pack("<I", len(body)))
        return {"stream": channel, "kind": kind, "body": body}

    def read_exactly(self, count):
        buffer = b""
        while len(buffer) < count:
            chunk = sys.stdin.buffer.read(count - len(buffer))
            if not chunk:
                return None
            buffer += chunk
        return buffer

    def hello(self, message):
        if self.mode == "old":
            self.write_plain({"jsonrpc": "2.0", "id": message["id"], "result": {
                "core": {"id": "06f3e992", "name": "mac"},
                "generation": 1, "framed": True, "version": "0.1.14",
                "features": ["framed"], "methods": HELLO_METHODS}})
            sys.exit(0)
        generation = 2 if self.mode == "gen2" else 1
        if self.mode == "gen2":
            self.write_plain({"jsonrpc": "2.0", "id": message["id"], "result": {
                "core": {"id": "06f3e992", "name": "mac"},
                "generation": generation, "framed": True, "version": "0.2.0",
                "features": ["framed"], "methods": HELLO_METHODS}})
            sys.exit(0)
        self.framed = True
        self.write_plain({"jsonrpc": "2.0", "id": message["id"], "result": {
            "core": {"id": "06f3e992", "name": "mac"},
            "generation": 1,
            "framed": True,
            "version": "0.1.21",
            "features": ["framed", "attach", "events", "status", "shell",
                         "resize_notifications", "automatic_input", "restart_environment"],
            "methods": HELLO_METHODS,
            "login_session": {"gui": True, "id": 100020, "user": 501},
            "keeps_running_after_logout": None,
            "logout_linger_user": None,
        }})

    def subscribe(self, message):
        self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
            "seq": 300,
            "terminals": list(self.terminals.values()),
            "channel": EVENT_CHANNEL,
        }})
        # One event right after the snapshot, as a live core does.
        self.write_event("event", {
            "seq": 301, "terminal": "t20", "type": "status",
            "data": {"state": "working", "app": "shprd-probe", "progress": 10,
                     "reported": True, "records": {}}})
        if self.mode == "missed":
            # A fresh set replacing the map, exactly as `missed` carries it.
            self.terminals["t23"] = terminal(
                "t23", {"herdr.space": SPACE, "herdr.tab": "tbprobe"},
                status={"state": "blocked", "kind": "permission",
                        "msg": "Approve?"}, turn=1)
            self.terminals.pop("t1y", None)
            self.write_event("missed", {
                "seq": 350, "terminals": list(self.terminals.values())})
        if self.mode == "doc":
            self.write_event("event", {
                "seq": 302, "type": "doc",
                "data": {"name": "herdr-app", "version": self.doc_version}})
        if self.mode == "die-mid":
            sys.exit(0)

    def answer(self, message):
        method = message.get("method")
        params = message.get("params") or {}
        self.log(method)
        if method == "hello":
            return self.hello(message)
        if method == "events.subscribe":
            return self.subscribe(message)
        if self.mode == "quiet":
            return
        if method == "terminal.list":
            self.list_calls += 1
            if self.mode == "die-once" and self.list_calls >= 2:
                sys.exit(0)
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "terminals": list(self.terminals.values()), "machines": [],
                "hidden_finished": 0}})
        if method == "doc.get":
            name = params.get("name")
            if name == "herdr-app":
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "value": LAYOUT, "version": self.doc_version}})
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "value": None, "version": 0}})
        if method == "server.status":
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "core": {"id": "06f3e992", "name": "mac"}, "pid": 2391, "live": 2,
                "version": "0.1.21"}})
        if method == "rpc.discover":
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "info": {"version": "0.1.21"},
                "methods": [{"name": name} for name in HELLO_METHODS]}})
        # Anything else keeps the client honest: a stable refusal, verbatim.
        return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "error": {
            "code": -32000,
            "message": f"no such method {method}",
            "data": {"kind": "invalid_params", "hint": "see hc schema"}}})

    def run(self):
        self.mark_spawn()
        while True:
            message = self.read_message()
            if message is None:
                return
            if "stream" in message:
                continue
            if message.get("method"):
                self.answer(message)


if __name__ == "__main__":
    fake = Fake()
    try:
        fake.run()
    except BrokenPipeError:
        pass
    finally:
        fake.mark_stop()
