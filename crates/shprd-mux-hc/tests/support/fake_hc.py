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
    <dir>/requests.jsonl  every request, whole, so a test can read the params
                          a verb sent (labels, environment, argv)
    <dir>/screen        what `terminal.read` answers, so a test drives a screen

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
    status-later
              answer normally; after a `terminal.get` for t20, report it `blocked`
              through the event stream, so a test sees a change arrive after the
              fold was already read
"""

import base64
import json
import os
import struct
import sys
import time

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
        self.documents = {}
        self.list_calls = 0
        self.next_terminal = 100

    def path(self, name):
        return os.path.join(self.dir, name)

    def read_mode(self):
        try:
            with open(self.path("mode")) as handle:
                return handle.read().strip() or "normal"
        except OSError:
            return "normal"

    def log(self, line, request=None):
        if not self.dir:
            return
        with open(self.path("log"), "a") as handle:
            handle.write(line + "\n")
        if request is not None:
            with open(self.path("requests.jsonl"), "a") as handle:
                handle.write(json.dumps(request) + "\n")

    def screen(self):
        try:
            with open(self.path("screen")) as handle:
                return handle.read()
        except OSError:
            return ""

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

    def log_stream(self, message):
        # What a client wrote on an attached stream: channel, frame kind, bytes.
        if message["kind"] == FRAME_CREDIT or not self.dir:
            return
        with open(self.path("streams.jsonl"), "a") as handle:
            handle.write(json.dumps({
                "channel": message["stream"], "kind": message["kind"],
                "data": message["body"].decode("utf-8", "replace")}) + "\n")

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
            "methods": [m for m in HELLO_METHODS if not (
                self.mode == "no-attach" and m == "terminal.attach")],
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
        # One event right after the snapshot, as a live core does. The record
        # moves with it, because a real core keeps the status in the terminal.
        self.terminals["t20"]["status"] = {"state": "working", "progress": 10,
                                           "app": "shprd-probe"}
        self.terminals["t20"]["reported"] = True
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

    def finish(self, address, reason="killed", exit_code=None, signal=1):
        record = self.terminals.get(address)
        if record is None:
            return None
        record["state"] = "finished"
        # A program that exits takes its status with it (M7ef §2.5).
        record["status"] = None
        record["reported"] = False
        record["reason"] = reason
        record["exit_code"] = exit_code
        record["signal"] = signal
        try:
            self.write_event("event", {
                "seq": 400, "terminal": address, "type": "finished",
                "data": {"exit_code": exit_code, "reason": reason,
                         "signal": signal, "finished_ms": 1794618609228}})
        except (BrokenPipeError, OSError):
            pass
        return record

    def terminal_run(self, message, params):
        address = f"t{self.next_terminal}"
        self.next_terminal += 1
        record = terminal(address, params.get("labels") or {})
        record["argv"] = params.get("argv") or []
        record["cwd"] = params.get("cwd") or "/tmp"
        self.terminals[address] = record
        return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                               "result": record})

    def terminal_of(self, params):
        wanted = params.get("terminal")
        return self.terminals.get(wanted), wanted

    def answer(self, message):
        method = message.get("method")
        params = message.get("params") or {}
        self.log(method, message)
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
        if method == "terminal.run":
            return self.terminal_run(message, params)
        if method in ("terminal.get", "terminal.kill", "terminal.rm",
                      "terminal.read", "terminal.send", "terminal.keys",
                      "terminal.wait"):
            record, wanted = self.terminal_of(params)
            if record is None:
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "error": {
                    "code": -32000, "message": f"no terminal {wanted!r}",
                    "data": {"kind": "not_found", "hint": "see hc ls --all"}}})
            if self.mode == "status-later" and method == "terminal.get" \
                    and wanted == "t20":
                record["status"] = {"state": "blocked", "kind": "question",
                                    "msg": "What now?"}
                record["turn"] = 2
                self.write_event("event", {
                    "seq": 320, "terminal": wanted, "type": "status",
                    "data": {"state": "blocked", "reported": True, "records": {}}})
            if method == "terminal.get":
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                                       "result": record})
            if method == "terminal.kill":
                self.finish(wanted)
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                                       "result": record})
            if method == "terminal.rm":
                self.terminals.pop(wanted, None)
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                                       "result": {"removed": wanted}})
            if method == "terminal.read":
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "text": self.screen(), "complete": True, "how": "screen"}})
            if method == "terminal.send":
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "status": None, "turn": 1, "started": None}})
            if method == "terminal.keys":
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "status": None, "turn": 1}})
            if method == "terminal.wait":
                output = params.get("output")
                if output and output in self.screen():
                    return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                        "terminal": wanted, "ended": False, "reason": "output",
                        "status": None, "turn": 1, "exit_code": None, "signal": None}})
                if record.get("state") == "finished":
                    return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                        "terminal": wanted, "ended": True, "reason": "exit",
                        "status": None, "turn": 1,
                        "exit_code": record.get("exit_code"), "signal": None}})
                time.sleep(0.05)
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "terminal": wanted, "ended": False, "reason": "timeout",
                    "status": None, "turn": 1, "exit_code": None, "signal": None}})
        if method == "terminal.attach":
            record, wanted = self.terminal_of(params)
            if record is None:
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "error": {
                    "code": -32000, "message": f"no terminal {wanted!r}",
                    "data": {"kind": "not_found"}}})
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "channel": 7, "automatic_input": True,
                "resize_notifications": False, "terminal": record}})
        if method == "doc.set":
            name = params.get("name")
            held = self.documents.get(name, 0)
            if params.get("version") != held:
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "error": {
                    "code": -32000,
                    "message": f"document {name!r} is at version {held}, not {params.get('version')}",
                    "data": {"kind": "conflict",
                             "hint": "read doc.get and retry with its version"}}})
            self.documents[name] = held + 1
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                                   "result": {"version": held + 1}})
        if method == "machine.exec":
            # A command named `binary-output` writes bytes that are not text, as a
            # real one summarizing a file may; everything else says `exec ok`.
            argv = params.get("argv") or []
            wrote = b"\xff\xfe exec ok" if argv[:1] == ["binary-output"] else b"exec ok"
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "result": {
                "code": 0, "stdout": base64.b64encode(wrote).decode(),
                "stderr": "", "ms": 1, "truncated": False, "timed_out": False}})
        if method == "terminal.label":
            record, wanted = self.terminal_of(params)
            if record is None:
                return self.write_rpc({"jsonrpc": "2.0", "id": message["id"], "error": {
                    "code": -32000, "message": "no terminal",
                    "data": {"kind": "not_found"}}})
            for key, value in (params.get("labels") or {}).items():
                if value is None:
                    record["labels"].pop(key, None)
                else:
                    record["labels"][key] = value
            return self.write_rpc({"jsonrpc": "2.0", "id": message["id"],
                                   "result": record})
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
                self.log_stream(message)
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
