"""Python runner for the generator comparison (see ../README.md).

Runs against whichever generator's `moq_ffi_gen` module is on the path; both bind the
same Rust and expose the same Python names. Prints a JSON report to stdout.

The throughput rounds mirror bench/ffi.py's: one publisher writes a group from this
task while one subscriber reads it on another, and every frame is checked byte for byte.
"""

import asyncio
import json
import os
import resource
import sys
import time
from typing import Awaitable, Callable

import moq_ffi_gen as gen

WARMUP = 3
CACHE_BYTES = 64 << 20
BUDGET = float(os.environ.get("MOQ_BENCH_SECONDS", "1"))
ROUND_TIMEOUT = 30
SIZES = (100, 1024, 16 * 1024, 64 * 1024, 1024 * 1024)


def cpu() -> float:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def frames(size: int) -> int:
    """Frames per group, holding a group under moq-net's 32 MB group cache."""
    return min(max((8 << 20) // size, 1), 64)


async def throughput(size: int, results: list[dict]) -> None:
    origin = gen.MoqOriginProducer(CACHE_BYTES)
    track = origin.create_broadcast("bench").publish_track("0")
    consumer = track.consume()
    # A non-uniform payload, so a zero-filled or truncated copy fails the check.
    payload = bytes(i % 251 for i in range(size))
    count = frames(size)

    async def read(n: int) -> None:
        group = await consumer.recv_group()
        for f in range(count):
            frame = await group.read_frame()
            assert frame is not None, "group ended"
            assert frame.payload == payload, f"corrupt frame {f} of round {n}"
            assert frame.timestamp_us == n * count + f

    async def step(n: int) -> None:
        reader = asyncio.create_task(read(n))
        group = track.append_group()
        for f in range(count):
            group.write_frame(gen.MoqFrame(payload=payload, timestamp_us=n * count + f))
        group.finish()
        await asyncio.wait_for(reader, ROUND_TIMEOUT)

    for n in range(WARMUP):
        await step(n)
    start, cpu_start, n = time.perf_counter(), cpu(), WARMUP
    while time.perf_counter() - start < BUDGET:
        await step(n)
        n += 1
    wall, used = time.perf_counter() - start, cpu() - cpu_start
    mb = (n - WARMUP) * count * size / 1e6
    results.append({"name": f"throughput/{size}", "unit": "MB/s", "value": mb / wall})
    results.append({"name": f"cpu/{size}", "unit": "ms/MB", "value": used * 1e3 / mb})


def call(name: str, fn: Callable[[], object], results: list[dict]) -> None:
    start, calls = time.perf_counter(), 0
    while time.perf_counter() - start < BUDGET:
        for _ in range(1000):
            fn()
        calls += 1000
    results.append({"name": f"call/{name}", "unit": "ns", "value": (time.perf_counter() - start) * 1e9 / calls})


async def call_async(name: str, fn: Callable[[], Awaitable[object]], results: list[dict]) -> None:
    start, calls = time.perf_counter(), 0
    while time.perf_counter() - start < BUDGET:
        for _ in range(1000):
            await fn()
        calls += 1000
    results.append({"name": f"call/{name}", "unit": "ns", "value": (time.perf_counter() - start) * 1e9 / calls})


async def ended() -> None:
    """A finished track surfaces as the slice's `Ended` error on both generators."""
    track = gen.MoqBroadcastProducer().publish_track("ended")
    consumer = track.consume()
    track.finish()
    try:
        await consumer.recv_group()
    except Exception as err:  # UniFFI raises MoqError.Ended, BoltFFI MoqErrorException.
        assert "Ended" in repr(err) or "Ended" in repr(getattr(err, "error", None)), repr(err)
        return
    raise AssertionError("recv_group on a finished track returned")


async def main() -> None:
    results: list[dict] = []
    await ended()
    for size in SIZES:
        await throughput(size, results)

    origin = gen.MoqOriginProducer(CACHE_BYTES)
    track = origin.create_broadcast("calls").publish_track("calls")
    group = track.append_group()
    consumer = track.consume()
    call("noop", gen.noop, results)
    call("sequence", group.sequence, results)
    call("info", consumer.info, results)

    # A fresh group every 1000 frames, so the origin's cache can evict the old ones.
    frame = gen.MoqFrame(payload=bytes(100), timestamp_us=0)
    writes = [group, 0]

    def write() -> None:
        writes[1] += 1
        if writes[1] % 1000 == 0:
            writes[0].finish()
            writes[0] = track.append_group()
        writes[0].write_frame(frame)

    call("write_frame/100", write, results)
    await call_async("noop_async", gen.noop_async, results)
    await call_async("yield_once", gen.yield_once, results)

    json.dump({"runner": f"python-{sys.argv[1]}", "results": results}, sys.stdout, indent=2)


if __name__ == "__main__":
    asyncio.run(main())
