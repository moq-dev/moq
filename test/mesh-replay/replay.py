#!/usr/bin/env python3
"""Replay a mesh of real moq-relay processes through a script of faults.

run.sh builds the binaries, reserves the ports, and calls this with them. This
starts one relay per mesh relay, one moq-shaper per link (the dialing relay
reaches its peer through it), and on every relay a publisher and a reader
(`moq-bench replay`). It then plays the script's events, measuring each from
the relays' /metrics and the readers' JSON lines, and writes report.json and
report.md. See README.md for the files and the report.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import zlib
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import tomllib

PREFIX = "replay"
POLL = 0.25
# How long a relay drains after SIGTERM, and how much longer a roll waits for it to exit.
DRAIN = 10
DRAIN_MARGIN = 20

# ── mesh and script ─────────────────────────────────────────────────────────


@dataclass
class Link:
    dial: str
    accept: str
    cost: int | None
    upstream: bool
    shape: dict
    # The build whose relays dial this link, or None for every build: a roll that also changes
    # the graph, as when the new build renders its dial set by a new rule.
    build: str | None = None

    @property
    def name(self) -> str:
        return f"{self.dial}-{self.accept}"

    def touches(self, relay: str) -> bool:
        return relay in (self.dial, self.accept)

    def other(self, relay: str) -> str:
        return self.accept if relay == self.dial else self.dial

    def dialed_on(self, build: str) -> bool:
        """Whether a dialer on `build` dials this link."""
        return self.build is None or self.build == build


@dataclass
class Mesh:
    name: str
    relays: list[str]
    links: list[Link]

    def neighbours(self, relay: str, build: str | None = None) -> list[str]:
        """The relays `relay` links on any build or, given one, with every relay on it."""
        return [
            link.other(relay) for link in self.links if link.touches(relay) and (build is None or link.dialed_on(build))
        ]

    def unreached(self, on: dict[str, str]) -> list[str]:
        """The relays the first can't reach when each relay runs the build `on` names."""
        links = [link for link in self.links if link.dialed_on(on[link.dial])]
        seen = {self.relays[0]}
        frontier = [self.relays[0]]
        while frontier:
            relay = frontier.pop()
            for link in links:
                if link.touches(relay) and link.other(relay) not in seen:
                    seen.add(link.other(relay))
                    frontier.append(link.other(relay))
        return [relay for relay in self.relays if relay not in seen]

    def builds(self) -> list[str | None]:
        """The builds whose graphs differ: old and new when a link names one, else just None."""
        return ["old", "new"] if any(link.build for link in self.links) else [None]

    def busiest(self) -> str:
        """The relay with the most links, the first listed among equals."""
        return max(self.relays, key=lambda relay: (len(self.neighbours(relay)), -self.relays.index(relay)))

    def diameter(self, build: str | None = None) -> int:
        """The most links any shortest path between two relays crosses, with every relay on `build`."""
        longest = 0
        for start in self.relays:
            hops = {start: 0}
            frontier = [start]
            while frontier:
                nxt = []
                for relay in frontier:
                    for neighbour in self.neighbours(relay, build):
                        if neighbour not in hops:
                            hops[neighbour] = hops[relay] + 1
                            nxt.append(neighbour)
                frontier = nxt
            if len(hops) != len(self.relays):
                on = f" on the {build} build" if build else ""
                raise ValueError(f"mesh is not connected{on}: {start} reaches only {sorted(hops)}")
            longest = max(longest, *hops.values())
        return longest


NAME = re.compile(r"^[a-z0-9][a-z0-9_]*$")
SHAPE_KEYS = {"delay", "rate", "queue"}
LINK_KEYS = {"dial", "accept", "cost", "upstream", "shape", "build"}


def load_mesh(path: Path) -> Mesh:
    doc = tomllib.loads(path.read_text())
    unknown = set(doc) - {"name", "relays", "shape", "links"}
    if unknown:
        raise ValueError(f"{path}: unknown keys {sorted(unknown)}")
    relays = doc.get("relays")
    if not isinstance(relays, list) or len(relays) < 2:
        raise ValueError(f"{path}: `relays` lists at least two relay names")
    for relay in relays:
        if not isinstance(relay, str) or not NAME.match(relay):
            raise ValueError(
                f"{path}: relay name {relay!r} is not lowercase letters, digits, or _ (a link is named dial-accept)"
            )
    if len(set(relays)) != len(relays):
        raise ValueError(f"{path}: a relay is listed twice")
    shape = doc.get("shape", {})
    if set(shape) - SHAPE_KEYS:
        raise ValueError(f"{path}: unknown shape keys {sorted(set(shape) - SHAPE_KEYS)}")
    links = []
    pairs = set()
    for raw in doc.get("links", []):
        if set(raw) - LINK_KEYS:
            raise ValueError(f"{path}: unknown link keys {sorted(set(raw) - LINK_KEYS)}")
        dial, accept = raw.get("dial"), raw.get("accept")
        if dial not in relays or accept not in relays or dial == accept:
            raise ValueError(f"{path}: link {dial!r} -> {accept!r} must join two listed relays")
        pair = frozenset((dial, accept))
        if pair in pairs:
            raise ValueError(f"{path}: {dial} and {accept} are joined twice")
        pairs.add(pair)
        cost = raw.get("cost")
        if cost is not None and (not isinstance(cost, int) or cost < 1):
            raise ValueError(f"{path}: link {dial}-{accept} cost must be a whole number of at least 1")
        own = raw.get("shape", {})
        if set(own) - SHAPE_KEYS:
            raise ValueError(f"{path}: unknown shape keys {sorted(set(own) - SHAPE_KEYS)}")
        build = raw.get("build")
        if build not in (None, "old", "new"):
            raise ValueError(f"{path}: link {dial}-{accept} build is `old` or `new`")
        upstream = bool(raw.get("upstream", False))
        if upstream and build == "old":
            raise ValueError(f"{path}: link {dial}-{accept} is marked upstream, which only the new build reads")
        links.append(Link(dial, accept, cost, upstream, {**shape, **own}, build))
    mesh = Mesh(doc.get("name", path.stem), relays, links)
    for build in mesh.builds():
        mesh.diameter(build)
    return mesh


KINDS = {"roll", "reopen-link", "reopen-relay", "kill", "end-source"}


@dataclass
class Script:
    start: str
    quiet: float
    settle: float
    down: float
    stall: float
    events: list[dict]
    known: list[dict]


def seconds(value, what: str) -> float:
    if isinstance(value, (int, float)) and value > 0:
        return float(value)
    match = re.fullmatch(r"(\d+(?:\.\d+)?)(ms|s|m)", str(value))
    if not match:
        raise ValueError(f"{what}: expected a duration such as 500ms, 10s or 2m, got {value!r}")
    scale = {"ms": 0.001, "s": 1.0, "m": 60.0}[match.group(2)]
    return float(match.group(1)) * scale


def load_script(path: Path, mesh: Mesh) -> Script:
    doc = tomllib.loads(path.read_text())
    unknown = set(doc) - {"start", "quiet", "settle", "down", "stall", "events", "known"}
    if unknown:
        raise ValueError(f"{path}: unknown keys {sorted(unknown)}")
    start = doc.get("start", "new")
    if start not in ("new", "old"):
        raise ValueError(f"{path}: start is `new` or `old`")
    events = []
    # Each relay's build as the script plays, so a graph that changes across a roll is checked
    # at every step rather than only on either build alone.
    on = dict.fromkeys(mesh.relays, start)
    for raw in doc.get("events", []):
        kind = raw.get("kind")
        if kind not in KINDS:
            raise ValueError(f"{path}: event kind {kind!r} is not one of {sorted(KINDS)}")
        event = dict(raw)
        if kind == "roll":
            if raw.get("to") not in ("new", "old"):
                raise ValueError(f"{path}: a roll goes `to` new or old")
            event["relays"] = raw.get("relays", mesh.relays)
            if not event["relays"] or set(event["relays"]) - set(mesh.relays):
                raise ValueError(f"{path}: a roll's `relays` lists relays of the mesh")
            for relay in event["relays"]:
                on[relay] = raw["to"]
                if cut := mesh.unreached(on):
                    raise ValueError(f"{path}: once {relay} rolls to {raw['to']}, {sorted(cut)} are cut off")
        if kind in ("reopen-relay", "kill", "end-source", "reopen-link"):
            relay = raw.get("relay", mesh.busiest())
            if relay not in mesh.relays:
                raise ValueError(f"{path}: {kind} names relay {relay!r}, not in the mesh")
            event["relay"] = relay
        if kind == "reopen-link":
            name = raw.get("link")
            if name is None:
                # One every build dials, so the fault never lands on a link nobody is dialing.
                link = next((link for link in mesh.links if link.touches(event["relay"]) and link.build is None), None)
                if link is None:
                    raise ValueError(f"{path}: every link of {event['relay']} is one build's; name the `link`")
            else:
                link = next((link for link in mesh.links if link.name == name), None)
                if link is None:
                    raise ValueError(
                        f"{path}: reopen-link names {name!r}; links are {[other.name for other in mesh.links]}"
                    )
                if not link.dialed_on(on[link.dial]):
                    raise ValueError(
                        f"{path}: reopen-link names {name}, which {link.dial} does not dial on its build then"
                    )
            event["link"] = link.name
        event["times"] = int(raw.get("times", 1))
        events.append(event)
    known = doc.get("known", [])
    for item in known:
        if set(item) - {"check", "kind", "event", "quest"} or "check" not in item or "quest" not in item:
            raise ValueError(f"{path}: a known entry has `check`, `quest`, and optionally `kind` and `event`")
    return Script(
        start=start,
        quiet=seconds(doc.get("quiet", "3s"), "quiet"),
        settle=seconds(doc.get("settle", "60s"), "settle"),
        down=seconds(doc.get("down", "15s"), "down"),
        stall=seconds(doc.get("stall", "3s"), "stall"),
        events=events,
        known=known,
    )


def ports_needed(mesh: Mesh) -> int:
    # Per relay its QUIC/HTTP port and its internal one, per link its shaper, one for auth.
    return 2 * len(mesh.relays) + len(mesh.links) + 1


# ── metrics ─────────────────────────────────────────────────────────────────

SAMPLE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{([^}]*)\})?\s+(\S+)")
LABEL = re.compile(r'(\w+)="((?:[^"\\]|\\.)*)"')

# The counters an event reports, each a key built from a metric and the labels that
# matter, summed over tiers. Every one is cumulative, so a fall means a restart.
COUNTERS = {
    "moq_relay_control_tx_messages_total": ("class",),
    "moq_relay_control_rx_messages_total": ("class",),
    "moq_relay_control_tx_bytes_total": ("class",),
    "moq_relay_control_rx_bytes_total": ("class",),
    "moq_relay_route_changes_total": (),
    "moq_relay_route_rewrites_total": (),
    "moq_relay_route_withdrawals_total": ("direction", "kind"),
    "moq_relay_route_refusals_total": ("reason",),
    "moq_relay_route_sessions_closed_total": ("kind",),
}

# What going quiet means: no route or announce message moving on any relay.
ANSI = re.compile(r"\x1b\[[0-9;]*m")
SESSION = re.compile(r'session="([0-9a-f]+)"')
NEGOTIATED = re.compile(r'session="([0-9a-f]+)".*\bnegotiated version=(\S+)')
GOAWAY = re.compile(r"received goaway", re.IGNORECASE)

QUIET = re.compile(r"^(moq_relay_route_|moq_relay_control_[rt]x_messages_total\|class=announce)")


def parse_metrics(text: str) -> dict[str, float]:
    """Sum each reported counter over every label but the ones COUNTERS keeps."""
    out: dict[str, float] = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        match = SAMPLE.match(line)
        if not match or match.group(1) not in COUNTERS:
            continue
        name = match.group(1)
        labels = dict(LABEL.findall(match.group(2) or ""))
        key = "|".join([name, *(f"{label}={labels.get(label, '')}" for label in COUNTERS[name])])
        out[key] = out.get(key, 0.0) + float(match.group(3))
    return out


class Totals:
    """One relay's counters accumulated across its restarts, and when they last moved."""

    def __init__(self) -> None:
        self.total: dict[str, float] = {}
        self.last: dict[str, float] = {}
        self.changed = 0.0
        self.seen: set[str] = set()

    def restart(self) -> None:
        self.last = {}

    def observe(self, sample: dict[str, float], now: float) -> None:
        for key, value in sample.items():
            self.seen.add(key)
            previous = self.last.get(key, 0.0)
            # A counter going down is a new process counting from zero.
            delta = value - previous if value >= previous else value
            if delta > 0:
                self.total[key] = self.total.get(key, 0.0) + delta
                if QUIET.match(key):
                    self.changed = now
            self.last[key] = value

    def snapshot(self) -> dict[str, float]:
        return dict(self.total)


def is_lite07(version: str) -> bool:
    """Whether a negotiated version is moq-lite-07, the wip draft or the final one."""
    return version == "moq-lite-07" or version.startswith("moq-lite-07-")


def delta(after: dict[str, float], before: dict[str, float]) -> dict[str, float]:
    return {key: value - before.get(key, 0.0) for key, value in after.items() if value != before.get(key, 0.0)}


def pick(counts: dict[str, float], name: str, **labels: str) -> int:
    key = "|".join([name, *(f"{label}={labels[label]}" for label in COUNTERS[name])])
    return int(counts.get(key, 0))


# ── readers ─────────────────────────────────────────────────────────────────


@dataclass
class Follow:
    online: bool = False
    groups: int = 0
    last: float | None = None
    gaps: list[tuple[float, float, int | None]] = field(default_factory=list)
    # Why the track of the broadcast handed out last ended, while nothing replaced it yet.
    ended: str | None = None
    # Each online (True) or offline (False) the follow reported, with when.
    changes: list[tuple[float, bool]] = field(default_factory=list)


class ReaderLog:
    """What one relay's reader reported, read incrementally from its JSON lines."""

    def __init__(self, path: Path) -> None:
        self.file = path
        self.offset = 0
        self.paths: dict[str, Follow] = {}
        self.status_at = 0.0

    def update(self) -> None:
        if not self.file.exists():
            return
        with self.file.open("rb") as f:
            f.seek(self.offset)
            data = f.read()
        end = data.rfind(b"\n")
        if end < 0:
            return
        self.offset += end + 1
        for raw in data[: end + 1].splitlines():
            self.apply(json.loads(raw))

    def apply(self, line: dict) -> None:
        ms = line["ms"] / 1000
        event = line["event"]
        if event == "status":
            self.status_at = ms
            for name, state in line["paths"].items():
                path = self.paths.setdefault(name, Follow())
                path.online = state["online"]
                path.groups = state["groups"]
                path.last = None if state["last"] is None else state["last"] / 1000
            return
        path = self.paths.setdefault(line["path"], Follow())
        if event == "gap":
            path.gaps.append((line["from"] / 1000, ms, line.get("missed")))
            path.last = ms
        elif event == "ended":
            path.ended = line["error"]
        elif event in ("online", "offline"):
            path.online = event == "online"
            path.ended = None
            path.changes.append((ms, path.online))


def withdrawn_twice(path: Follow, start: float, end: float) -> bool:
    """Whether the follow went offline, came back, and went offline again within [start, end]:
    a withdrawal that stepped through End, Start, End instead of ending once."""
    want = [False, True, False]
    for at, online in path.changes:
        if start <= at <= end and online == want[0]:
            want.pop(0)
            if not want:
                return True
    return False


def interruption(path: Follow, start: float, end: float, gap: float, known: float | None = None) -> tuple[float, int]:
    """The longest pause in arrivals overlapping [start, end], and the groups it missed. `known` is
    when the reader last reported, so a pause still going is measured to then, not to `end`."""
    longest, missed = 0.0, 0
    for begin, finish, lost in path.gaps:
        if finish >= start and begin <= end:
            longest = max(longest, finish - begin)
            missed += lost or 0
    # A pause still going at the end of the window counts up to it; a path never fed, all of it.
    if path.last is None:
        longest = end - start
    else:
        # A gap logged after the status line moved `last` past it, so that pause already ended.
        still = min(end, max(known if known is not None else end, path.last)) - path.last
        if still > gap:
            longest = max(longest, still)
    return longest, missed


# ── processes ───────────────────────────────────────────────────────────────


class Proc:
    """A child process in this run's process group, logging to one file across restarts."""

    def __init__(self, name: str, log: Path) -> None:
        self.name = name
        self.log = log
        self.proc: subprocess.Popen | None = None
        # Whether the run means it to be running: set by start, cleared by `end` and `stop`.
        self.wanted = False

    def start(self, argv: list[str], env: dict | None = None) -> None:
        assert self.proc is None or self.proc.poll() is not None, f"{self.name} is already running"
        self.wanted = True
        with self.log.open("ab") as out:
            out.write(f"\n==== {time.strftime('%H:%M:%S')} start {' '.join(argv)}\n".encode())
            self.proc = subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, env=env)

    @property
    def alive(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    @property
    def died(self) -> bool:
        """Exited while the run meant it to be running."""
        return self.wanted and not self.alive

    def end(self, sig: int) -> None:
        """Signal the process to exit, as the run means it to."""
        self.wanted = False
        self.send(sig)

    def send(self, sig: int) -> None:
        if self.alive:
            assert self.proc is not None
            self.proc.send_signal(sig)

    def wait(self, timeout: float) -> int | None:
        if self.proc is None:
            return None
        try:
            return self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            return None

    def stop(self, sig: int = signal.SIGINT, timeout: float = 10.0) -> None:
        self.wanted = False
        if not self.alive:
            return
        self.send(sig)
        if self.wait(timeout) is None:
            self.send(signal.SIGKILL)
            self.wait(5)


def http_get(url: str, timeout: float = 2.0) -> str | None:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return response.read().decode()
    except (urllib.error.URLError, OSError, TimeoutError):
        return None


class Auth:
    """The relays' auth server: a link session presents the token `peer.<relay>` and is granted a
    peer link, every other session the whole tree. It also logs every session event."""

    def __init__(self, port: int, log: Path) -> None:
        self.events: list[dict] = []
        self.lock = threading.Lock()
        events, lock = self.events, self.lock
        out = log.open("a")

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args) -> None:
                pass

            def do_POST(self) -> None:
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
                query = dict(part.split("=", 1) for part in (body.get("query") or "").split("&") if "=" in part)
                # lite-07 carries the token in SETUP, older versions as `?jwt=`.
                token = query.get("jwt", "")
                if body.get("token"):
                    value = body["token"]["value"]
                    token = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4)).decode(errors="replace")
                record = {
                    "t": time.time(),
                    "event": body.get("event"),
                    "id": body.get("id"),
                    "local": body.get("local"),
                    "peer": token.removeprefix("peer.") if token.startswith("peer.") else None,
                }
                with lock:
                    events.append(record)
                    out.write(json.dumps(record) + "\n")
                    out.flush()
                if body.get("event") == "end":
                    self.send_response(204)
                    self.end_headers()
                    return
                grant = {"publish": ["**"], "subscribe": ["**"]}
                if record["peer"]:
                    grant |= {"link": "peer", "peer": True}
                payload = json.dumps(grant).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.server.shutdown()


# ── the run ─────────────────────────────────────────────────────────────────


@dataclass
class Build:
    name: str
    binary: str
    versions: list[str]
    version: str = ""


class Negotiated:
    """The version each session of a relay's running process negotiated, read from its log."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.offset = 0
        self.sessions: dict[str, str] = {}

    def restart(self) -> None:
        """Forget the last process: called before a new one starts writing."""
        self.offset = self.path.stat().st_size if self.path.exists() else 0
        self.sessions = {}

    def update(self) -> None:
        if not self.path.exists():
            return
        with self.path.open("rb") as f:
            f.seek(self.offset)
            data = f.read()
        # Up to the last whole line, so a line being written is read whole next time.
        whole = data.rfind(b"\n") + 1
        self.offset += whole
        for line in ANSI.sub("", data[:whole].decode(errors="replace")).splitlines():
            if m := NEGOTIATED.search(line):
                self.sessions[m.group(1)] = m.group(2)


class Relay:
    def __init__(self, run: Run, name: str, port: int, internal: int) -> None:
        self.run = run
        self.name = name
        self.port = port
        self.internal = internal
        self.proc = Proc(f"relay {name}", run.dir / f"relay-{name}.log")
        self.negotiated = Negotiated(self.proc.log)
        self.build: Build | None = None
        # When the running process started, so a link counts only the sessions it opened.
        self.started = 0.0
        self.totals = Totals()
        self.publisher = Proc(f"publisher {name}", run.dir / f"publisher-{name}.log")
        self.reader = Proc(f"reader {name}", run.dir / f"reader-{name}.log")
        self.readout = ReaderLog(run.dir / f"reader-{name}.jsonl")

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/"

    def config(self, build: Build) -> str:
        run = self.run
        peers = []
        for link in run.mesh.links:
            if link.dial != self.name or not link.dialed_on(build.name):
                continue
            entry = f'url = "https://127.0.0.1:{run.shaper_port[link.name]}/", token = "peer.{self.name}"'
            if link.cost is not None:
                entry += f", cost = {link.cost}"
            # The mark is the new build's config: a release before it rejects the field, so the old
            # build's link is unmarked, as a deployment rolling off that release configures it.
            if link.upstream and build.name == "new":
                entry += ", upstream = true"
            peers.append("{ " + entry + " }")
        versions = ""
        if build.versions:
            versions = "version = [" + ", ".join(f'"{v}"' for v in build.versions) + "]"
        return f"""# Generated by test/mesh-replay/replay.py for relay {self.name} ({build.name} build).
# Set rather than defaulted: a release before 0.17.18 drained for 0s, skipping the GOAWAY a roll sends.
drain_timeout = "{DRAIN}s"

[log]
level = "{run.log}"

[listen]
bind = "127.0.0.1:{self.port}"
tls.generate = ["localhost", "127.0.0.1"]
{versions}

[connect]
tls.insecure = true
{versions}

[web.http]
listen = "127.0.0.1:{self.port}"

[internal]
listen = "127.0.0.1:{self.internal}"

[auth]
url = "http://127.0.0.1:{run.auth_port}/"

[stats]
enabled = true
node = "{self.name}"

[cluster]
connect = [{", ".join(peers)}]
"""

    def start(self, build: Build) -> None:
        path = self.run.dir / f"relay-{self.name}-{build.name}.toml"
        path.write_text(self.config(build))
        self.build = build
        # Under the poller's lock, so a scrape of the process this replaces is never counted
        # again against the cleared counters.
        with self.run.lock:
            self.totals.restart()
        self.negotiated.restart()
        self.started = time.time()
        self.proc.start([build.binary, str(path)])
        deadline = time.time() + 30
        while time.time() < deadline:
            if not self.proc.alive:
                raise RuntimeError(f"relay {self.name} exited on start; see {self.proc.log}")
            if http_get(f"http://127.0.0.1:{self.internal}/metrics") is not None:
                return
            time.sleep(0.1)
        raise RuntimeError(f"relay {self.name} never served /metrics; see {self.proc.log}")

    def poll(self, now: float) -> None:
        if not self.proc.alive:
            return
        text = http_get(f"http://127.0.0.1:{self.internal}/metrics", timeout=1.0)
        if text is not None:
            self.totals.observe(parse_metrics(text), now)

    def announced(self) -> set[str] | None:
        text = http_get(f"{self.url}announced/{PREFIX}")
        if text is None:
            return None
        return {f"{PREFIX}/{line.strip()}" for line in text.splitlines() if line.strip()}

    def paths(self) -> list[str]:
        return [f"{PREFIX}/{self.name}/{i}" for i in range(self.run.broadcasts)]

    def followed(self) -> list[str]:
        """The broadcasts every other relay's reader follows: the first `--follow`, or all."""
        return self.paths()[: self.run.follow or None]

    def client(self, proc: Proc, flags: list[str]) -> None:
        build = self.run.builds["new"]
        versions = []
        for version in build.versions:
            versions += ["--connect-version", version]
        proc.start([self.run.bench, "replay", "--connect", self.url, *versions, *flags])

    def start_publisher(self) -> None:
        flags = []
        for path in self.paths():
            flags += ["--publish", path]
        self.client(self.publisher, flags)

    def start_reader(self) -> None:
        flags = ["--output", str(self.readout.file)]
        for relay in self.run.relays.values():
            if relay is not self:
                for path in relay.followed():
                    flags += ["--follow", path]
        self.client(self.reader, flags)


class Run:
    def __init__(self, args, mesh: Mesh, script: Script) -> None:
        self.dir = Path(args.dir)
        self.mesh = mesh
        self.script = script
        self.broadcasts = args.broadcasts
        self.follow = args.follow
        self.log = args.log
        self.bench = args.bench
        self.shaper = args.shaper
        self.builds = {
            "new": Build("new", args.new, [v for v in args.new_versions.split(",") if v]),
            "old": Build("old", args.old or args.new, [v for v in args.old_versions.split(",") if v]),
        }
        ports = [int(port) for port in args.ports.split(",")]
        if len(ports) < ports_needed(mesh):
            raise ValueError(f"need {ports_needed(mesh)} ports, got {len(ports)}")
        self.auth_port = ports.pop()
        self.shaper_port = {link.name: ports.pop() for link in mesh.links}
        self.relays = {name: Relay(self, name, ports.pop(), ports.pop()) for name in mesh.relays}
        self.shapers = {
            link.name: Proc(f"shaper {link.name}", self.dir / f"shaper-{link.name}.log") for link in mesh.links
        }
        self.auth = Auth(self.auth_port, self.dir / "auth.jsonl")
        self.events: list[dict] = []
        self.violations: list[dict] = []
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.poller = threading.Thread(target=self.poll, daemon=True)

    # ── plumbing ──

    def poll(self) -> None:
        while not self.stop.is_set():
            now = time.time()
            for relay in self.relays.values():
                with self.lock:
                    relay.poll(now)
            time.sleep(POLL)

    def snapshot(self) -> dict[str, dict[str, float]]:
        now = time.time()
        with self.lock:
            for relay in self.relays.values():
                relay.poll(now)
            return {name: relay.totals.snapshot() for name, relay in self.relays.items()}

    def changed(self) -> float:
        return max(relay.totals.changed for relay in self.relays.values())

    def start_shaper(self, link: Link) -> None:
        shape = link.shape
        accept = self.relays[link.accept]
        profile = self.dir / f"shaper-{link.name}.toml"
        direction = []
        if "delay" in shape:
            direction.append(f'delay = "{shape["delay"]}"')
        rate = ""
        if "rate" in shape:
            rate = f'bits_per_second = {int(shape["rate"])}\nburst = 12500\nqueue = "{shape.get("queue", "100ms")}"\n'
        # Shared: every flow on the link, a predecessor's and its replacement's alike, draws on one
        # rate limit and queue each way, as one physical link would.
        body = f'name = "{link.name}"\nseed = {zlib.crc32(link.name.encode()) % 1000}\nshared = true\n'
        for side in ("up", "down"):
            body += f"\n[{side}]\n" + "\n".join(direction) + "\n"
            if rate:
                body += f"\n[{side}.rate]\n{rate}"
        profile.write_text(body)
        self.shapers[link.name].start(
            [
                self.shaper,
                "--listen",
                f"127.0.0.1:{self.shaper_port[link.name]}",
                "--target",
                f"127.0.0.1:{accept.port}",
                "--profile",
                str(profile),
                "--report",
                str(self.dir / f"shaper-{link.name}.json"),
            ]
        )
        time.sleep(0.2)
        if not self.shapers[link.name].alive:
            raise RuntimeError(f"shaper {link.name} exited on start; see {self.shapers[link.name].log}")

    def versions(self) -> dict[str, str]:
        """Each dialed link's version, as its acceptor negotiated it on the newest open session the
        dialer's running process opened there, or `down` when it holds none or is not running. A
        link its dialer's build does not dial is left out."""
        with self.auth.lock:
            sessions = [e for e in self.auth.events if e["peer"]]
        ended = {e["id"] for e in sessions if e["event"] == "end"}
        for relay in self.relays.values():
            relay.negotiated.update()
        out = {}
        for link in self.mesh.links:
            dialer = self.relays[link.dial]
            if dialer.build is None or not link.dialed_on(dialer.build.name):
                continue
            accepted = self.relays[link.accept].negotiated.sessions
            version = "down"
            if dialer.proc.alive:
                for e in sessions:
                    # An acceptor holds a dead process's session until its idle timeout.
                    fresh = e["t"] >= dialer.started
                    if e["peer"] == link.dial and fresh and e["id"] in accepted and e["id"] not in ended:
                        version = accepted[e["id"]]
            out[link.name] = version
        return out

    def readers(self) -> None:
        for relay in self.relays.values():
            relay.readout.update()

    # ── events ──

    def measure(self, name: str, kind: str, apply, targets: set[str], **extra) -> dict:
        """Apply one fault, wait for the mesh to go quiet, and report what it cost."""
        self.readers()
        before = self.snapshot()
        versions_before = self.versions()
        start = time.time()
        print(f"[{time.strftime('%H:%M:%S')}] {name}", flush=True)
        restored = apply()
        quiet_at, fed_at, unfed, unrouted = self.settle(restored, self.baseline(kind))
        end = time.time()
        after = self.snapshot()
        self.readers()
        versions = self.versions()
        lite07 = {name for v in (versions_before, versions) for name, version in v.items() if is_lite07(version)}
        on07 = {r for link in self.mesh.links if link.name in lite07 for r in (link.dial, link.accept)}

        deltas = {relay: delta(after[relay], before[relay]) for relay in self.relays}
        total: dict[str, float] = {}
        for relay, counts in deltas.items():
            for key, value in counts.items():
                if key.startswith("moq_relay_route_") and relay not in on07:
                    continue
                total[key] = total.get(key, 0.0) + value

        report = {
            "name": name,
            "kind": kind,
            **extra,
            "start": round(start, 3),
            "fault_s": round(restored - start, 2),
            "quiet_s": None if quiet_at is None else round(max(0.0, quiet_at - restored), 2),
            "fed_s": None if fed_at is None else round(max(0.0, fed_at - restored), 2),
            "announce": {
                f"{way}_{unit}": pick(total, f"moq_relay_control_{way}_{unit}_total", **{"class": "announce"})
                for way in ("tx", "rx")
                for unit in ("messages", "bytes")
            },
            "routes": {
                "changed": pick(total, "moq_relay_route_changes_total"),
                "rewritten": pick(total, "moq_relay_route_rewrites_total"),
                "sent": {
                    k: pick(total, "moq_relay_route_withdrawals_total", direction="sent", kind=k)
                    for k in ("dead", "drain")
                },
                "received": {
                    k: pick(total, "moq_relay_route_withdrawals_total", direction="received", kind=k)
                    for k in ("dead", "drain")
                },
                "refused": {
                    k: v
                    for k in ("path", "scope", "looped")
                    if (v := pick(total, "moq_relay_route_refusals_total", reason=k))
                },
                "closed": {
                    k: v
                    for k in ("stalled", "overrun", "malformed", "role", "unasked", "regressed", "other")
                    if (v := pick(total, "moq_relay_route_sessions_closed_total", kind=k))
                },
                "relays": sorted(on07),
                "unavailable": sorted(set(self.relays) - on07),
            },
            "per_relay": {relay: {k: int(v) for k, v in counts.items()} for relay, counts in deltas.items() if counts},
            "versions": versions,
            "readers": self.reader_report(start, end, targets),
        }
        self.check(report, unfed, unrouted)
        self.events.append(report)
        return report

    def baseline(self, kind: str) -> bool:
        """Whether every reader must have a route: at the start, before any fault, unless an
        upstream mark its dialer's build applies (the new build's only) may hide one."""
        return kind == "start" and not any(
            link.upstream and getattr(self.relays[link.dial].build, "name", None) == "new" for link in self.mesh.links
        )

    def settle(self, restored: float, routed: bool) -> tuple[float | None, float | None, list[dict], list[dict]]:
        """Wait, up to `settle`, until no route or announce counter moved for `quiet` and every
        reader with a route is reading, and with `routed`, every reader has a route. Returns when
        each happened, or None, and who was not."""
        deadline = restored + self.script.settle
        quiet_at = fed_at = None
        while True:
            now = time.time()
            unfed, unrouted = self.hungry(now, restored)
            if unfed or (routed and unrouted):
                fed_at = None
            elif fed_at is None:
                fed_at = now
            # After reading the readers, which takes a while, so counters that moved meanwhile count.
            last = max(self.changed(), restored)
            quiet_at = last if time.time() - last >= self.script.quiet else None
            if (quiet_at is not None and fed_at is not None) or now >= deadline:
                return quiet_at, fed_at, unfed, unrouted
            time.sleep(POLL * 2)

    def reader_report(self, start: float, end: float, targets: set[str]) -> dict:
        gap = 0.5
        rows = []
        for relay in self.relays.values():
            # A path's newest arrival is known only as of the reader's latest status line, up to a
            # report interval old, so a pause still going is measured to that line, not to now.
            known = max(start, min(end, relay.readout.status_at))
            for path, state in relay.readout.paths.items():
                source = path.split("/")[1]
                longest, missed = interruption(state, start, end, gap, known)
                rows.append(
                    {
                        "reader": relay.name,
                        "path": path,
                        "s": longest,
                        "missed": missed,
                        "withdrawn_twice": withdrawn_twice(state, start, end),
                        "bystander": source not in targets and relay.name not in targets,
                    }
                )

        def summary(group: list[dict]) -> dict:
            hit = sorted((row["s"] for row in group if row["s"] > gap), reverse=True)
            return {
                "readers": len(group),
                "interrupted": len(hit),
                "max_s": round(hit[0], 2) if hit else 0.0,
                "p50_s": round(hit[len(hit) // 2], 2) if hit else 0.0,
                "missed": sum(row["missed"] for row in group),
                "withdrawn_twice": sum(row["withdrawn_twice"] for row in group),
            }

        worst = sorted(rows, key=lambda row: -row["s"])[:5]
        return {
            "all": summary(rows),
            "bystanders": summary([row for row in rows if row["bystander"]]),
            "worst": [{**row, "s": round(row["s"], 2)} for row in worst if row["s"] > gap],
            "withdrawn_twice": [
                {"reader": row["reader"], "path": row["path"]} for row in rows if row["withdrawn_twice"]
            ][:5],
        }

    def hungry(self, now: float, restored: float) -> tuple[list[dict], list[dict]]:
        """Readers whose relay shows their live broadcast but who read nothing since `restored` or
        for `stall`, and readers whose live broadcast their relay does not show."""
        live = {relay.name for relay in self.relays.values() if relay.proc.alive}
        publishing = {relay.name for relay in self.relays.values() if relay.publisher.alive}
        unfed, unrouted = [], []
        self.readers()
        for relay in self.relays.values():
            shows = relay.announced() if relay.name in live else None
            if shows is None:
                continue
            # Every path the reader was told to follow, so one it never reported counts too, and
            # every live broadcast for the route, followed or not.
            for other in self.relays.values():
                if other is relay or other.name not in publishing or other.name not in live:
                    continue
                followed = set(other.followed())
                for path in other.paths():
                    state = relay.readout.paths.get(path) or Follow()
                    if path not in shows:
                        unrouted.append({"reader": relay.name, "path": path})
                    elif path not in followed:
                        continue
                    elif state.last is None or state.last < restored or now - state.last > self.script.stall:
                        unfed.append(
                            {
                                "reader": relay.name,
                                "path": path,
                                "since_s": None if state.last is None else round(now - state.last, 1),
                                # Stuck: the track of the broadcast it holds ended and the follow
                                # never handed out another. Starved: it holds one whose groups stopped.
                                "kind": "stuck" if state.ended is not None else "starved",
                                "ended": state.ended,
                            }
                        )
        return unfed, unrouted

    def check(self, report: dict, unfed: list[dict], unrouted: list[dict]) -> None:
        """Record each reader with a route left unfed, and each broadcast shown with no source."""
        publishing = {relay.name for relay in self.relays.values() if relay.publisher.alive}
        shown = []
        for relay in self.relays.values():
            shows = relay.announced() if relay.proc.alive else None
            for path in shows or ():
                if path.split("/")[1] not in publishing:
                    shown.append({"relay": relay.name, "path": path})
        report["unrouted"] = len(unrouted)
        if unrouted:
            report["unrouted_detail"] = unrouted[:20]
            # A baseline with a reader left unrouted tests nothing behind it.
            if self.baseline(report["kind"]):
                self.violation(report, "unrouted", unrouted)
        for kind in ("stuck", "starved"):
            items = [item for item in unfed if item["kind"] == kind]
            if items:
                self.violation(report, "unfed", items, kind)
        if shown:
            self.violation(report, "shown", shown)
        died = [proc.name for proc in self.procs() if proc.died]
        if died:
            self.violation(report, "died", died)

    def violation(self, report: dict, check: str, items: list, kind: str | None = None) -> None:
        # The start is the baseline before any fault, so no known flaw excuses it.
        known = next(
            (
                k
                for k in self.script.known
                if report["kind"] != "start"
                and k["check"] == check
                and k.get("kind", kind) == kind
                and k.get("event", report["kind"]) == report["kind"]
            ),
            None,
        )
        entry = {"event": report["name"], "check": check, "count": len(items), "items": items[:20]}
        if kind:
            entry["kind"] = kind
        if known:
            entry["known"] = known["quest"]
        report.setdefault("violations", []).append(entry)
        self.violations.append(entry)

    # ── faults ──

    def roll(self, to: str, relays: list[str], suffix: str = "") -> None:
        build = self.builds[to]
        for name in relays:
            relay = self.relays[name]
            # The links it holds as it drains: each one its dialer's build dials.
            neighbours = [
                link.other(name)
                for link in self.mesh.links
                if link.touches(name) and link.dialed_on(self.relays[link.dial].build.name)
            ]
            offsets = {n: self.relays[n].proc.log.stat().st_size for n in neighbours}
            before = self.snapshot()

            hung: list[dict] = []

            def apply(relay=relay, hung=hung) -> float:
                relay.proc.end(signal.SIGTERM)
                # Keep polling while it drains, so its last counts are kept, but no longer than the
                # drain allows: a relay that never exits is killed and reported, not waited on.
                deadline = time.time() + DRAIN + DRAIN_MARGIN
                while relay.proc.wait(POLL) is None:
                    if time.time() > deadline:
                        relay.proc.send(signal.SIGKILL)
                        relay.proc.wait(5)
                        hung.append({"relay": relay.name, "after_s": DRAIN + DRAIN_MARGIN})
                        break
                relay.start(build)
                return time.time()

            report = self.measure(f"roll {name} to {to}{suffix}", "roll", apply, {name}, relay=name, to=to)
            if hung:
                self.violation(report, "hung", hung)
            after = self.snapshot()
            report["links"] = [self.drained(name, n, offsets[n], before, after) for n in neighbours]

    def drained(self, draining: str, neighbour: str, offset: int, before: dict, after: dict) -> dict:
        """Whether `neighbour` heard a GOAWAY from `draining` on their link, read from its log, and
        from its GOAWAY counter where its build has one."""
        link = next(x for x in self.mesh.links if x.touches(draining) and x.touches(neighbour))
        with self.relays[neighbour].proc.log.open("rb") as f:
            f.seek(offset)
            text = ANSI.sub("", f.read().decode(errors="replace"))
        if link.dial == neighbour:
            # The neighbour dialed: its reconnect loop logs the GOAWAY with the peer's URL, and
            # a release's dial logs it on a session with no span. Only `draining` drains now,
            # so a GOAWAY on a session the neighbour dialed is its.
            url = f"127.0.0.1:{self.shaper_port[link.name]}"
            logged = any(
                ("upstream GOAWAY" in line and url in line) or (GOAWAY.search(line) and not SESSION.search(line))
                for line in text.splitlines()
            )
        else:
            # The draining relay dialed: the neighbour logs it on the session the auth server
            # admitted for that dialer.
            with self.auth.lock:
                sessions = {e["id"] for e in self.auth.events if e["peer"] == draining}
            logged = any(
                (m := SESSION.search(line)) and m.group(1) in sessions
                for line in text.splitlines()
                if GOAWAY.search(line)
            )
        key = "moq_relay_control_rx_messages_total|class=goaway"
        counted = None
        if self.relays[neighbour].build is self.builds["new"]:
            counted = after[neighbour].get(key, 0.0) - before[neighbour].get(key, 0.0) > 0
        return {
            "link": link.name,
            "neighbour": neighbour,
            "dialed_by": link.dial,
            "saw": "drain" if logged else "dead",
            # The neighbour's GOAWAY counter, a second reading where its build counts one.
            "counted": counted,
        }

    def reopen(self, links: list[Link]) -> float:
        for link in links:
            self.shapers[link.name].send(signal.SIGSTOP)
        time.sleep(self.script.down)
        for link in links:
            self.shapers[link.name].send(signal.SIGCONT)
        return time.time()

    def run_event(self, event: dict) -> None:
        kind = event["kind"]
        for attempt in range(event["times"]):
            suffix = f" ({attempt + 1}/{event['times']})" if event["times"] > 1 else ""
            if kind == "roll":
                self.roll(event["to"], event["relays"], suffix)
                continue
            relay = self.relays[event["relay"]]
            if kind == "reopen-link":
                link = next(x for x in self.mesh.links if x.name == event["link"])
                self.measure(
                    f"reopen link {link.name}{suffix}",
                    kind,
                    lambda: self.reopen([link]),
                    {link.dial, link.accept},
                    link=link.name,
                )
            elif kind == "reopen-relay":
                links = [x for x in self.mesh.links if x.touches(relay.name)]
                self.measure(
                    f"reopen every link of {relay.name}{suffix}",
                    kind,
                    lambda: self.reopen(links),
                    {relay.name},
                    relay=relay.name,
                )
            elif kind == "kill":

                def apply() -> float:
                    build = relay.build
                    relay.proc.end(signal.SIGKILL)
                    relay.proc.wait(5)
                    time.sleep(self.script.down)
                    assert build is not None
                    relay.start(build)
                    return time.time()

                self.measure(f"kill and restart {relay.name}{suffix}", kind, apply, {relay.name}, relay=relay.name)
            elif kind == "end-source":

                def end() -> float:
                    relay.publisher.stop()
                    return time.time()

                self.measure(f"end {relay.name}'s broadcasts{suffix}", kind, end, {relay.name}, relay=relay.name)

                def restart() -> float:
                    relay.start_publisher()
                    return time.time()

                self.measure(
                    f"restart {relay.name}'s broadcasts{suffix}",
                    "restart-source",
                    restart,
                    {relay.name},
                    relay=relay.name,
                )

    def execute(self) -> None:
        self.poller.start()
        start = self.builds[self.script.start]
        for link in self.mesh.links:
            self.start_shaper(link)
        for relay in self.relays.values():
            relay.start(start)
        for relay in self.relays.values():
            relay.start_publisher()
            relay.start_reader()
        self.measure("start", "start", time.time, set(self.relays))
        for event in self.script.events:
            self.run_event(event)

        # Every source ends: nothing may stay shown, and the mesh must then hold still.
        def end_all() -> float:
            for relay in self.relays.values():
                relay.publisher.stop()
            return time.time()

        self.measure("end every broadcast", "end-source", end_all, set(self.relays))
        before = self.snapshot()
        time.sleep(max(self.script.quiet * 2, 5.0))
        moved = {
            relay: {k: int(v) for k, v in delta(self.snapshot()[relay], before[relay]).items() if QUIET.match(k)}
            for relay in self.relays
        }
        moved = {relay: counts for relay, counts in moved.items() if counts}
        if moved:
            entry = {"event": "after the script", "check": "noisy", "count": len(moved), "items": [moved]}
            self.violations.append(entry)
        died = [proc.name for proc in self.procs() if proc.died]
        if died:
            self.violations.append({"event": "after the script", "check": "died", "count": len(died), "items": died})

    def procs(self) -> list[Proc]:
        """Every child process: relays, publishers, readers, and shapers."""
        out = [proc for relay in self.relays.values() for proc in (relay.proc, relay.publisher, relay.reader)]
        return out + list(self.shapers.values())

    def teardown(self) -> None:
        self.stop.set()
        procs = [r.reader for r in self.relays.values()] + [r.publisher for r in self.relays.values()]
        for proc in procs:
            proc.send(signal.SIGINT)
        for proc in procs:
            proc.stop(timeout=5)
        for relay in self.relays.values():
            relay.proc.stop(signal.SIGTERM, timeout=15)
        for shaper in self.shapers.values():
            shaper.send(signal.SIGCONT)
            shaper.stop(timeout=5)
        self.auth.close()

    def links_report(self) -> list[dict]:
        out = []
        for link in self.mesh.links:
            path = self.dir / f"shaper-{link.name}.json"
            try:
                stats = json.loads(path.read_text()) if path.exists() else {}
            except (OSError, ValueError) as err:
                # Written in place, so a shaper killed mid-write leaves it torn: report that, not crash.
                stats = {"error": f"{path.name}: {err}"}
            out.append(
                {"link": link.name, "cost": link.cost, "upstream": link.upstream, "build": link.build, "shaper": stats}
            )
        return out


# ── report ──────────────────────────────────────────────────────────────────


def star(link: dict, event: dict) -> str:
    """A rolled relay's link, starred when the rolled relay dialed it."""
    return link["link"] + ("*" if link["dialed_by"] == event["relay"] else "")


COLUMNS = (
    "Event",
    "Quiet (s)",
    "Fed (s)",
    "Announce msgs / bytes",
    "Changed",
    "Rewritten",
    "DEAD sent/recv",
    "DRAIN sent/recv",
    "Readers hit (bystanders)",
    "Longest (s)",
    "Missed",
    "End, Start, End",
)

NOTE = (
    "Quiet is how long after the fault ended until the last route or announce message moved, once "
    "nothing more moved for the script's quiet window, and Fed until every reader whose relay shows "
    "its live broadcast read a group sent after the fault; `-` never settled. Message counts are "
    "summed over every relay's sent side. Route counts "
    "cover moq-lite-07 relays only (n/a when none). Readers hit counts reader paths that paused "
    "longer than 0.5s; bystanders are readers whose relay and broadcast source were both outside "
    "the fault. End, Start, End counts reader paths whose broadcast went offline, came back, and "
    "went offline again within the event, a withdrawal that did not end once."
)


def row(cells) -> str:
    return "| " + " | ".join(str(cell) for cell in cells) + " |"


def seconds_cell(value: float | None) -> str:
    return "-" if value is None else f"{value:.1f}"


def event_row(event: dict) -> str:
    routes, readers, announce = event["routes"], event["readers"], event["announce"]

    def route(*keys: str) -> str:
        if not routes["relays"]:
            return "n/a"
        value = routes
        for key in keys:
            value = value[key]
        return str(value)

    return row(
        [
            event["name"],
            seconds_cell(event["quiet_s"]),
            seconds_cell(event["fed_s"]),
            f"{announce['tx_messages']} / {announce['tx_bytes']}",
            route("changed"),
            route("rewritten"),
            f"{route('sent', 'dead')}/{route('received', 'dead')}",
            f"{route('sent', 'drain')}/{route('received', 'drain')}",
            f"{readers['all']['interrupted']} ({readers['bystanders']['interrupted']})",
            f"{readers['all']['max_s']:.1f}",
            readers["all"]["missed"],
            readers["all"]["withdrawn_twice"],
        ]
    )


def graph(mesh: dict) -> str:
    """The mesh's links and diameter, per build when a roll changes the graph."""
    if set(mesh["diameter"]) == {"all"}:
        return f"{len(mesh['links'])} links, diameter {mesh['diameter']['all']}"
    return "; ".join(
        f"{sum(1 for link in mesh['links'] if link['build'] in (None, build))} links on the {build} build, "
        f"diameter {mesh['diameter'][build]}"
        for build in ("old", "new")
    )


def version_rows(events: list[dict]) -> list[str]:
    """A row per event that changed which version a link negotiated: the links on each version
    after it, and the links that moved (`-` where the link was not dialed)."""
    out = []
    previous: dict[str, str] = {}
    for event in events:
        current = event.get("versions", {})
        if current == previous:
            continue
        counts = {}
        for version in current.values():
            counts[version] = counts.get(version, 0) + 1
        moved = [
            f"{name}: {previous.get(name, '-')} to {current.get(name, '-')}"
            for name in sorted(set(previous) | set(current))
            if previous.get(name) != current.get(name)
        ]
        summary = ", ".join(f"{version} {n}" for version, n in sorted(counts.items()))
        # The first row would list every link as moved; its counts say the same.
        out.append(row([event["name"], summary or "-", "; ".join(moved) if previous else "-"]))
        previous = current
    return out


def markdown(report: dict) -> str:
    mesh, builds = report["mesh"], report["builds"]
    result = "passed" if report["passed"] else "failed"
    if report["known"]:
        result += f", {len(report['known'])} known"
    lines = [
        f"## Mesh replay: {mesh['name']}, {len(mesh['relays'])} relays, {report['broadcasts']} broadcasts each"
        + (
            f", readers follow {report['follow']} of each"
            if report.get("follow", report["broadcasts"]) < report["broadcasts"]
            else ""
        ),
        "",
        f"{graph(mesh)}. New build `{builds['new']}`, old build `{builds['old']}`.",
        f"Result: **{result}**",
        "",
        row(COLUMNS),
        row(["---"] * len(COLUMNS)),
        *(event_row(event) for event in report["events"]),
    ]
    if report.get("error"):
        lines += ["", f"The run stopped early: {report['error']}"]
    versions = version_rows(report["events"])
    if versions:
        lines += ["", "Links on each version after each event that changed one, as the accepting relay negotiated:", ""]
        lines += [row(["Event", "Links per version", "Moved"]), row(["---"] * 3), *versions]
    rolls = [event for event in report["events"] if event.get("links")]
    if rolls:
        lines += [
            "",
            "Links of each rolled relay, as its neighbour saw the drain (`*` dialed by the rolled relay):",
            "",
        ]
        lines += [row(["Roll", "GOAWAY heard (DRAIN)", "Closed only (DEAD)"]), row(["---"] * 3)]
        for event in rolls:
            heard = [star(link, event) for link in event["links"] if link["saw"] == "drain"]
            closed = [star(link, event) for link in event["links"] if link["saw"] == "dead"]
            lines.append(row([event["name"], ", ".join(heard) or "-", ", ".join(closed) or "-"]))
        disagree = [
            f"{event['name']}: {link['link']}"
            for event in rolls
            for link in event["links"]
            if link["counted"] is not None and link["counted"] != (link["saw"] == "drain")
        ]
        if disagree:
            lines += ["", f"The GOAWAY counter disagrees with the log on: {', '.join(disagree)}."]
    if report["violations"] or report["known"]:
        lines += ["", "Violations:", ""]
        for entry in report["violations"] + report["known"]:
            tag = f" (known: {entry['known']})" if "known" in entry else ""
            check = entry["check"] + (f" ({entry['kind']})" if entry.get("kind") else "")
            lines.append(f"- {entry['event']}: {check} x{entry['count']}{tag}: `{json.dumps(entry['items'][:3])}`")
    lines += ["", NOTE, ""]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    need = sub.add_parser("ports", help="print how many ports a mesh needs")
    need.add_argument("mesh", type=Path)
    run = sub.add_parser("run", help="run the script against the mesh")
    run.add_argument("--mesh", type=Path, required=True)
    run.add_argument("--script", type=Path, required=True)
    run.add_argument("--broadcasts", type=int, required=True)
    run.add_argument("--follow", type=int, default=0, help="broadcasts of each relay a reader follows (0: all)")
    run.add_argument("--dir", required=True)
    run.add_argument("--ports", required=True)
    run.add_argument("--bench", required=True)
    run.add_argument("--shaper", required=True)
    run.add_argument("--new", required=True)
    run.add_argument("--new-label", default="", help="what the new build is, such as a commit")
    run.add_argument("--new-versions", default="moq-lite-07-wip,moq-lite-06")
    run.add_argument("--old", default="")
    run.add_argument("--old-versions", default="")
    run.add_argument("--report", type=Path, required=True)
    run.add_argument("--log", default="info", help="the relays' log level")
    args = parser.parse_args()
    if args.command == "run" and args.follow < 0:
        parser.error("--follow must be 0 (all) or more")

    if args.command == "ports":
        print(ports_needed(load_mesh(args.mesh)))
        return 0

    mesh = load_mesh(args.mesh)
    script = load_script(args.script, mesh)
    if not args.old and any(e["kind"] == "roll" for e in script.events):
        raise SystemExit("the script rolls between builds: pass --old")
    run = Run(args, mesh, script)
    for build in run.builds.values():
        out = subprocess.run([build.binary, "--version"], capture_output=True, text=True, check=True)
        build.version = out.stdout.strip()
    if args.new_label:
        run.builds["new"].version += f" ({args.new_label})"

    def interrupted(signum, frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    error = None
    try:
        run.execute()
    except Exception as err:
        error = f"{type(err).__name__}: {err}"
        print(f"error: {error}", file=sys.stderr)
    finally:
        run.teardown()

    failing = [v for v in run.violations if "known" not in v]
    report = {
        "mesh": {
            "name": mesh.name,
            "relays": mesh.relays,
            "links": run.links_report(),
            "diameter": {build or "all": mesh.diameter(build) for build in mesh.builds()},
        },
        "broadcasts": args.broadcasts,
        "follow": min(args.follow or args.broadcasts, args.broadcasts),
        "builds": {name: build.version for name, build in run.builds.items()},
        "events": run.events,
        "violations": failing,
        "known": [v for v in run.violations if "known" in v],
        "error": error,
        "passed": not failing and error is None,
    }
    args.report.mkdir(parents=True, exist_ok=True)
    (args.report / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    text = markdown(report)
    (args.report / "report.md").write_text(text)
    print(text)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
