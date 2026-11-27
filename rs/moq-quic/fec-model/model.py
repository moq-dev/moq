#!/usr/bin/env python3
"""Experimental: how late do video frames arrive on one reliable QUIC stream over a lossy
link, with and without forward error correction?

A discrete-event model, not moq-quic: it has RFC 9002's loss detection (packet threshold,
time threshold, PTO) and RFC 9000's delayed ACKs, a paced application-limited sender, and no
congestion controller. It exists to compare FEC designs cheaply before any is built into the
transport, so every code is idealised: a block of k sources and r repairs decodes once any k
of them arrive (MDS), and a sliding-window code decodes whenever its equations have full
rank over a large field.

The sender never learns that FEC recovered a packet, so it still retransmits it. That costs
the retransmission's bytes and keeps the loss visible to congestion control (RFC 9265).

Run `python3 model.py` for the default sweep, or `--help`.
"""

import argparse
import heapq
import json
import math
import random
import sys
from concurrent.futures import ProcessPoolExecutor
from dataclasses import asdict, dataclass

PRIME = (1 << 31) - 1

PACKET_THRESHOLD = 3
TIME_THRESHOLD = 9 / 8
MAX_ACK_DELAY = 0.025
ACK_EVERY = 2


class Gilbert:
    """A link that is good or bad for exponentially distributed spells, losing `bad_loss` of
    the packets sent while it is bad and `good_loss` of the rest. The spells are in time,
    not packets, so every sender sees the same fades however it paces."""

    def __init__(self, seed, bad_ms, good_ms, bad_loss, good_loss):
        self.spells = random.Random(seed)
        self.drops = random.Random(seed + 1)
        self.mean_bad = bad_ms / 1000
        self.mean_good = good_ms / 1000
        self.bad_loss, self.good_loss = bad_loss, good_loss
        self.bad = False
        self.until = self.spells.expovariate(1 / self.mean_good) if bad_ms > 0 else math.inf

    def lost(self, now):
        while now >= self.until:
            self.bad = not self.bad
            mean = self.mean_bad if self.bad else self.mean_good
            self.until += self.spells.expovariate(1 / mean)
        rate = self.bad_loss if self.bad else self.good_loss
        return rate > 0 and self.drops.random() < rate


# Encoders. Each returns the repairs to send, as tuples of the source chunks they cover.


class NoFec:
    name = "none"

    def on_source(self, chunk):
        return []

    def on_frame_end(self):
        return []


class Block:
    """`k` sources then `r` repairs; a frame's end closes a partial block when `flush`."""

    def __init__(self, k, r, flush):
        self.k, self.r, self.flush = k, r, flush
        self.sources = []

    def close(self):
        if not self.sources:
            return []
        sources, self.sources = tuple(self.sources), []
        # A partial block keeps the same redundancy ratio, never below one repair.
        repairs = max(1, round(self.r * len(sources) / self.k)) if self.k else self.r
        return [("block", sources, repairs)] * repairs

    def on_source(self, chunk):
        self.sources.append(chunk)
        return self.close() if self.k and len(self.sources) >= self.k else []

    def on_frame_end(self):
        return self.close() if self.flush else []


class FrameBlock(Block):
    """One block per frame, with repairs in proportion to its size."""

    def __init__(self, ratio):
        super().__init__(0, 0, True)
        self.ratio = ratio

    def close(self):
        if not self.sources:
            return []
        sources, self.sources = tuple(self.sources), []
        repairs = max(1, round(self.ratio * len(sources)))
        return [("block", sources, repairs)] * repairs


class Overlap:
    """A block per frame end covering the last `span` frames, so each frame is covered
    `span` times, the later ones after whatever fade hit it has passed."""

    def __init__(self, ratio, span):
        self.ratio, self.span = ratio, span
        self.frames = [[]]

    def on_source(self, chunk):
        self.frames[-1].append(chunk)
        return []

    def on_frame_end(self):
        newest = len(self.frames[-1])
        sources = tuple(c for frame in self.frames for c in frame)
        self.frames = self.frames[-(self.span - 1) :] + [[]] if self.span > 1 else [[]]
        if not newest:
            return []
        repairs = max(1, round(self.ratio * newest))
        return [("block", sources, repairs)] * repairs


class Interleave:
    """`m` XOR blocks filled round-robin, so a burst of up to `m` packets costs each one loss."""

    def __init__(self, m, k):
        self.blocks = [Block(k, 1, True) for _ in range(m)]
        self.next = 0

    def on_source(self, chunk):
        out = self.blocks[self.next].on_source(chunk)
        self.next = (self.next + 1) % len(self.blocks)
        return out

    def on_frame_end(self):
        return [repair for block in self.blocks for repair in block.on_frame_end()]


class Sliding:
    """A repair after every `n` sources, a random combination of the last `w`; a frame's end
    sends `tail` more."""

    def __init__(self, n, w, tail):
        self.n, self.w, self.tail = n, w, tail
        self.window = []
        self.since = 0

    def repair(self):
        return ("linear", tuple(self.window), 0)

    def on_source(self, chunk):
        self.window.append(chunk)
        del self.window[: -self.w]
        self.since += 1
        if self.since >= self.n:
            self.since = 0
            return [self.repair()]
        return []

    def on_frame_end(self):
        self.since = 0
        return [self.repair() for _ in range(self.tail)]


def encoder(spec):
    kind, *args = spec.split(":")
    if kind == "none":
        return NoFec()
    if kind == "block":
        return Block(int(args[0]), int(args[1]), len(args) > 2 and args[2] == "flush")
    if kind == "frame":
        return FrameBlock(float(args[0]))
    if kind == "overlap":
        return Overlap(float(args[0]), int(args[1]))
    if kind == "interleave":
        return Interleave(int(args[0]), int(args[1]))
    if kind == "sliding":
        return Sliding(int(args[0]), int(args[1]), int(args[2]) if len(args) > 2 else 0)
    raise ValueError(spec)


class Decoder:
    """Recovers chunks from repairs. `have` is the receiver's set of available chunks."""

    def __init__(self, have, rng):
        self.have = have
        self.rng = rng
        self.blocks = {}  # sources -> [repairs received, repairs sent]
        self.by_chunk = {}  # chunk -> sources of each block seen covering it
        self.rows = {}  # pivot chunk -> {chunk: coefficient}, in reduced row echelon form

    def try_block(self, sources):
        missing = [c for c in sources if c not in self.have]
        return missing if 0 < len(missing) <= self.blocks[sources][0] else []

    def on_repair(self, repair):
        kind, sources, _ = repair
        if kind == "block":
            entry = self.blocks.get(sources)
            if entry is None:
                entry = self.blocks[sources] = [0]
                for chunk in sources:
                    self.by_chunk.setdefault(chunk, []).append(sources)
            entry[0] += 1
            return self.try_block(sources)
        row = {c: self.rng.randrange(1, PRIME) for c in sources if c not in self.have}
        return self.insert(row)

    def on_source(self, chunk):
        """A chunk arrived or was recovered: what does knowing it recover?"""
        out = []
        for sources in self.by_chunk.get(chunk, ()):
            out += self.try_block(sources)
        if self.rows:
            row = self.rows.pop(chunk, None)
            if row is not None:
                # Its equation still constrains the other unknowns.
                del row[chunk]
                out += self.insert(row)
            for pivot in [p for p, r in self.rows.items() if chunk in r]:
                del self.rows[pivot][chunk]
            out += self.solved()
        return out

    def insert(self, row):
        for pivot in [p for p in row if p in self.rows]:
            factor = row.pop(pivot)
            for chunk, coefficient in self.rows[pivot].items():
                if chunk != pivot:
                    value = (row.get(chunk, 0) - factor * coefficient) % PRIME
                    if value:
                        row[chunk] = value
                    else:
                        row.pop(chunk, None)
        if not row:
            return []
        pivot = min(row)
        inverse = pow(row[pivot], PRIME - 2, PRIME)
        row = {c: v * inverse % PRIME for c, v in row.items()}
        for other in self.rows.values():
            factor = other.pop(pivot, 0)
            if factor:
                for chunk, coefficient in row.items():
                    if chunk != pivot:
                        value = (other.get(chunk, 0) - factor * coefficient) % PRIME
                        if value:
                            other[chunk] = value
                        else:
                            other.pop(chunk, None)
        self.rows[pivot] = row
        return self.solved()

    def solved(self):
        done = [p for p, r in self.rows.items() if len(r) == 1]
        for pivot in done:
            del self.rows[pivot]
        return done


@dataclass
class Config:
    fec: str = "none"
    rtt_ms: float = 60
    bad_ms: float = 0.25  # mean bad spell; 0 for a link with no bad state
    good_ms: float = 12.25  # mean good spell
    bad_loss: float = 1.0
    good_loss: float = 0.0
    fps: float = 30
    packets: float = 14  # mean packets per frame
    packets_sd: float = 4
    pace_mbps: float = 20
    packet_bytes: int = 1200
    rttvar_ratio: float = 1 / 16
    frames: int = 20000
    seed: int = 1


def run(cfg):
    rng = random.Random(cfg.seed)
    forward = Gilbert(cfg.seed * 7919 + 1, cfg.bad_ms, cfg.good_ms, cfg.bad_loss, cfg.good_loss)
    reverse = Gilbert(cfg.seed * 7919 + 5, cfg.bad_ms, cfg.good_ms, cfg.bad_loss, cfg.good_loss)
    delay = cfg.rtt_ms / 2000
    srtt = 2 * delay
    loss_delay = TIME_THRESHOLD * srtt
    pto = srtt + 4 * cfg.rttvar_ratio * srtt + MAX_ACK_DELAY
    interval = cfg.packet_bytes * 8 / (cfg.pace_mbps * 1e6)
    period = 1 / cfg.fps

    events = []
    order = 0

    def at(time, kind, data=None):
        nonlocal order
        order += 1
        heapq.heappush(events, (time, order, kind, data))

    # Frames: frame f owns chunks [first[f], first[f + 1]).
    sizes = [max(1, round(rng.gauss(cfg.packets, cfg.packets_sd))) for _ in range(cfg.frames)]
    first = [0]
    for size in sizes:
        first.append(first[-1] + size)
    frame_of = [f for f, size in enumerate(sizes) for _ in range(size)]
    chunks = first[-1]
    for f in range(cfg.frames):
        at(f * period, "frame", f)

    # Sender.
    enc = encoder(cfg.fec)
    new, rtx, repairs = [], [], []
    new_head = 0
    rtx_queued = set()
    sent = []  # [time, kind, payload, state]: state 0 outstanding, 1 acked, 2 lost
    outstanding = {}
    chunk_acked = bytearray(chunks)
    next_send = 0.0
    send_armed = False
    largest_acked = -1
    processed = 0
    last_eliciting = 0.0
    backoff = 0
    pto_gen = 0
    loss_gen = 0
    lost_packets = 0
    counts = {"source": 0, "rtx": 0, "repair": 0, "probe": 0, "rtx_redundant": 0, "recovered": 0}

    # Receiver.
    have = set()
    dec = Decoder(have, random.Random(cfg.seed * 7919 + 9))
    recv_log = []
    largest_recv = -1
    unacked = 0
    ack_gen = 0
    ack_armed = False
    in_order = 0
    got = [0] * cfg.frames
    alone_done = [math.inf] * cfg.frames
    ordered_done = [math.inf] * cfg.frames
    next_frame = 0

    def arm_pto(now):
        nonlocal pto_gen
        pto_gen += 1
        if outstanding:
            at(max(now, last_eliciting + pto * (1 << backoff)), "pto", pto_gen)

    def transmit(now, kind, payload):
        nonlocal last_eliciting, lost_packets
        pn = len(sent)
        sent.append([now, kind, payload, 0])
        outstanding[pn] = None
        last_eliciting = now
        if forward.lost(now):
            lost_packets += 1
        else:
            at(now + delay, "arrive", pn)
        arm_pto(now)

    def pump(now):
        nonlocal next_send, send_armed, new_head
        while rtx or repairs or new_head < len(new):
            if now < next_send:
                if not send_armed:
                    send_armed = True
                    at(next_send, "send")
                return
            if rtx:
                chunk = rtx.pop(0)
                rtx_queued.discard(chunk)
                if chunk_acked[chunk]:
                    continue
                counts["rtx"] += 1
                transmit(now, "data", chunk)
            elif repairs:
                counts["repair"] += 1
                transmit(now, "repair", repairs.pop(0))
            else:
                chunk = new[new_head]
                new_head += 1
                counts["source"] += 1
                transmit(now, "data", chunk)
                repairs.extend(enc.on_source(chunk))
                if chunk + 1 == first[frame_of[chunk] + 1]:
                    repairs.extend(enc.on_frame_end())
            next_send = now + interval

    def detect(now):
        nonlocal loss_gen
        earliest = math.inf
        for pn in list(outstanding):
            if pn >= largest_acked:
                break
            time, kind, payload, _ = sent[pn]
            if largest_acked - pn >= PACKET_THRESHOLD or now >= time + loss_delay:
                sent[pn][3] = 2
                del outstanding[pn]
                if kind == "data" and not chunk_acked[payload] and payload not in rtx_queued:
                    rtx_queued.add(payload)
                    rtx.append(payload)
            else:
                earliest = min(earliest, time + loss_delay)
        if earliest < math.inf:
            loss_gen += 1
            at(earliest, "loss", loss_gen)

    def available(now, chunk, recovered):
        nonlocal in_order, next_frame
        pending = [chunk]
        while pending:
            chunk = pending.pop()
            if chunk in have:
                continue
            have.add(chunk)
            if recovered:
                counts["recovered"] += 1
            recovered = True
            f = frame_of[chunk]
            got[f] += 1
            if got[f] == sizes[f]:
                alone_done[f] = now
            pending += dec.on_source(chunk)
        while in_order < chunks and in_order in have:
            in_order += 1
        while next_frame < cfg.frames and in_order >= first[next_frame + 1]:
            ordered_done[next_frame] = now
            next_frame += 1

    def ack(now):
        nonlocal unacked, ack_armed, ack_gen
        unacked = 0
        ack_armed = False
        ack_gen += 1
        if not reverse.lost(now):
            at(now + delay, "ack", len(recv_log))

    while events:
        now, _, kind, data = heapq.heappop(events)
        if kind == "frame":
            new.extend(range(first[data], first[data + 1]))
            pump(now)
        elif kind == "send":
            send_armed = False
            pump(now)
        elif kind == "arrive":
            pn = data
            _, what, payload, _ = sent[pn]
            recv_log.append(pn)
            gap = pn != largest_recv + 1
            largest_recv = max(largest_recv, pn)
            if what == "data":
                if payload in have:
                    counts["rtx_redundant"] += 1
                available(now, payload, False)
            elif what == "repair":
                for chunk in dec.on_repair(payload):
                    available(now, chunk, True)
            unacked += 1
            if gap or unacked >= ACK_EVERY:
                ack(now)
            elif not ack_armed:
                ack_armed = True
                at(now + MAX_ACK_DELAY, "ack_timer", ack_gen)
        elif kind == "ack_timer":
            if data == ack_gen and unacked:
                ack(now)
        elif kind == "ack":
            newly = False
            for pn in recv_log[processed:data]:
                entry = sent[pn]
                largest_acked = max(largest_acked, pn)
                if entry[3] == 0:
                    del outstanding[pn]
                    newly = True
                if entry[3] != 1:
                    entry[3] = 1
                    if entry[1] == "data":
                        chunk_acked[entry[2]] = 1
            processed = max(processed, data)
            if newly:
                backoff = 0
            detect(now)
            arm_pto(now)
            pump(now)
        elif kind == "loss":
            if data == loss_gen:
                detect(now)
                pump(now)
        elif kind == "pto":
            if data == pto_gen and outstanding:
                backoff += 1
                # Probe with the oldest data still unacknowledged, as a retransmission.
                probe = next((sent[pn][2] for pn in outstanding if sent[pn][1] == "data" and not chunk_acked[sent[pn][2]]), None)
                counts["probe"] += 1
                transmit(now, "data" if probe is not None else "ping", probe)

    base = delay
    ordered = sorted(ordered_done[f] - f * period - base for f in range(cfg.frames))
    alone = sorted(alone_done[f] - f * period - base for f in range(cfg.frames))

    def pct(values, q):
        return values[min(len(values) - 1, int(q * len(values)))] * 1000

    late = period  # a frame that misses the next frame's slot is a visible stall
    return {
        **asdict(cfg),
        "overhead": counts["repair"] / counts["source"],
        "loss": lost_packets / len(sent),
        "rtx": counts["rtx"] / counts["source"],
        "rtx_redundant": counts["rtx_redundant"] / max(1, counts["rtx"]),
        "recovered": counts["recovered"] / counts["source"],
        "probes": counts["probe"],
        "p50": pct(ordered, 0.50),
        "p95": pct(ordered, 0.95),
        "p99": pct(ordered, 0.99),
        "p999": pct(ordered, 0.999),
        "max": ordered[-1] * 1000,
        "late": sum(1 for v in ordered if v > late) / cfg.frames,
        "alone_p99": pct(alone, 0.99),
        "alone_p999": pct(alone, 0.999),
        "alone_late": sum(1 for v in alone if v > late) / cfg.frames,
    }


def single(loss):
    """Losses one packet long: a 0.25 ms spell at the default pacing."""
    return dict(bad_ms=0.25, good_ms=0.25 * (1 - loss) / loss)


def spells(loss, bad_ms):
    return dict(bad_ms=bad_ms, good_ms=bad_ms * (1 - loss) / loss)


# Each link as Gilbert parameters. The named profiles are a synthesis from published
# measurements (README.md), converted from per-packet chains at 417 packets a second.
LINKS = {
    "1% single": single(0.01),
    "2% single": single(0.02),
    "5% single": single(0.05),
    "2% in 2 ms spells": spells(0.02, 2),
    "2% in 8 ms spells": spells(0.02, 8),
    "wifi contended": dict(bad_ms=16, good_ms=800, bad_loss=0.6, good_loss=0.001),
    "cellular mobile": dict(bad_ms=24, good_ms=3000, bad_loss=0.8, good_loss=0.0002),
    "wifi roam": dict(bad_ms=55, good_ms=60000, bad_loss=1.0, good_loss=0.0005),
}

CODES = [
    "none",
    "block:7:1:flush",
    "interleave:2:7",
    "frame:0.1",
    "frame:0.2",
    "frame:0.3",
    "overlap:0.1:2",
    "overlap:0.2:2",
    "overlap:0.3:2",
    "overlap:0.2:3",
    "sliding:10:30:1",
    "sliding:7:28:2",
]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--rtt", type=float, nargs="+", default=[20, 60, 150], help="round trip times, ms")
    parser.add_argument("--links", nargs="+", default=list(LINKS), choices=list(LINKS))
    parser.add_argument("--pace", type=float, default=20, help="pacing rate, Mbit/s; the video is 4")
    parser.add_argument("--frames", type=int, default=20000)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--codes", nargs="+", default=CODES)
    parser.add_argument("--json", help="also write every run to this file")
    parser.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()

    configs = [
        (name, Config(fec=code, rtt_ms=rtt, pace_mbps=args.pace, frames=args.frames, seed=args.seed, **LINKS[name]))
        for rtt in args.rtt
        for name in args.links
        for code in args.codes
    ]
    with ProcessPoolExecutor(args.jobs) as pool:
        results = list(pool.map(run, [cfg for _, cfg in configs]))

    rows = []
    last = None
    for (name, cfg), r in zip(configs, results):
        r["link"] = name
        rows.append(r)
        key = (cfg.rtt_ms, name)
        if key != last:
            last = key
            print(f"\nRTT {cfg.rtt_ms:.0f} ms, {name} ({r['loss']:.2%} lost): frame lateness beyond the one-way delay, ms")
            print(f"{'code':<18}{'over':>6}{'p50':>7}{'p95':>7}{'p99':>7}{'p99.9':>7}{'max':>7}{'late':>8}{'alone99':>9}{'alone-late':>11}{'rtx':>7}{'wasted':>8}")
        print(
            f"{cfg.fec:<18}{r['overhead']:>6.0%}{r['p50']:>7.1f}{r['p95']:>7.1f}{r['p99']:>7.1f}{r['p999']:>7.1f}{r['max']:>7.0f}"
            f"{r['late']:>8.2%}{r['alone_p99']:>9.1f}{r['alone_late']:>11.2%}{r['rtx']:>7.1%}{r['rtx_redundant']:>8.0%}"
        )
    if args.json:
        with open(args.json, "w") as out:
            json.dump(rows, out, indent=1)


if __name__ == "__main__":
    sys.exit(main())
