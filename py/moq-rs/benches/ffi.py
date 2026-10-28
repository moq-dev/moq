"""Python runner of the binding benchmark (see bench/README.md).

The same in-process rounds as every wrapper, so dividing by the Rust baselines
isolates what Python adds: the FFI boundary, asyncio bridging, and the wrapper.
"""

import asyncio
import json
import os
import resource
import time
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path
from typing import Callable

import moq

WARMUP = 3
# Each case's origin cache budget: far above one round's groups, so eviction only
# reclaims groups every subscriber has read, and memory stays flat over a long run.
CACHE_BYTES = 64 << 20
BUDGET = float(os.environ.get("MOQ_BENCH_SECONDS", "1"))
# Far longer than any honest round, so only a lost wakeup trips it.
ROUND_TIMEOUT = 30


@dataclass(frozen=True)
class Case:
    """One throughput case: `publishers` tracks, each read by `subscribers` consumers."""

    size: int
    publishers: int
    subscribers: int

    @property
    def frames(self) -> int:
        """Frames per group, holding a group under moq-net's 32 MB group cache."""
        return min(max((8 << 20) // self.size, 1), 64)

    @property
    def delivered(self) -> int:
        return self.publishers * self.subscribers * self.frames * self.size

    @property
    def name(self) -> str:
        return f"size={self.size}/pub={self.publishers}/sub={self.subscribers}"


# Frame sizes at one publisher and subscriber, then fan-out at 1 KiB.
CASES = [Case(size, 1, 1) for size in (100, 1024, 64 * 1024, 1024 * 1024)] + [
    Case(1024, p, s) for p, s in ((1, 4), (4, 1), (4, 4))
]


def cpu() -> float:
    """Process CPU seconds, user plus system, across every thread."""
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


async def read(case: Case, consumer: moq.TrackConsumer, payload: bytes, n: int) -> None:
    group = await consumer.recv_group()
    assert group is not None, "track ended"
    for f in range(case.frames):
        frame = await group.read_frame()
        assert frame is not None, "group ended"
        assert frame.payload == payload, f"corrupt frame {f} of round {n}"
        assert frame.timestamp == timedelta(microseconds=n * case.frames + f)


async def throughput(case: Case, results: list[dict]) -> None:
    origin = moq.OriginProducer(cache_capacity_bytes=CACHE_BYTES)
    broadcast = origin.create_broadcast("bench").broadcast
    tracks = [broadcast.publish_track(str(p), moq.TrackInfo(timescale=1_000_000)) for p in range(case.publishers)]
    consumers = [track.consume() for track in tracks for _ in range(case.subscribers)]
    # A non-uniform payload, so a zero-filled or truncated copy fails the check.
    payload = bytes(i % 251 for i in range(case.size))

    async def step(n: int) -> None:
        # Each consumer reads on its own task while this one writes each group in turn.
        readers = [asyncio.create_task(read(case, consumer, payload, n)) for consumer in consumers]
        for track in tracks:
            group = track.append_group()
            for f in range(case.frames):
                group.write_frame(payload, timedelta(microseconds=n * case.frames + f))
            group.finish()
        await asyncio.wait_for(asyncio.gather(*readers), ROUND_TIMEOUT)

    for n in range(WARMUP):
        await step(n)
    start, cpu_start, n = time.perf_counter(), cpu(), WARMUP
    while time.perf_counter() - start < BUDGET:
        await step(n)
        n += 1
    wall, cpu_used = time.perf_counter() - start, cpu() - cpu_start
    mb = (n - WARMUP) * case.delivered / 1e6
    results.append({"name": f"throughput/{case.name}", "unit": "MB/s", "value": mb / wall})
    results.append({"name": f"cpu/{case.name}", "unit": "ms/MB", "value": cpu_used * 1e3 / mb})


def call(name: str, fn: Callable[[], object], results: list[dict]) -> None:
    start, calls = time.perf_counter(), 0
    while time.perf_counter() - start < BUDGET:
        for _ in range(1000):
            fn()
        calls += 1000
    results.append({"name": f"call/{name}", "unit": "ns", "value": (time.perf_counter() - start) * 1e9 / calls})


async def main() -> None:
    results: list[dict] = []
    for case in CASES:
        await throughput(case, results)

    track = moq.BroadcastProducer().publish_track("calls")
    group = track.append_group().consume()
    call("sequence", lambda: group.sequence, results)
    consumer = track.consume()
    call("info", consumer.info, results)

    report = json.dumps(results, indent=2)
    if out := os.environ.get("MOQ_BENCH_DIR"):
        Path(out).mkdir(parents=True, exist_ok=True)
        (Path(out) / "python.json").write_text(report)
    else:
        print(report)


if __name__ == "__main__":
    asyncio.run(main())
