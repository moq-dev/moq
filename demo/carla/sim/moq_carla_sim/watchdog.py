"""End the worker when it stops making progress without failing.

CARLA's synchronous-mode Traffic Manager can deadlock `world.tick()` inside the
client library (carla-simulator/carla#9172), before the RPC whose timeout would
otherwise raise. The clock thread then blocks forever, the world freezes, and
the next blocking CARLA call from the event loop never returns either. Every
asyncio watcher in `supervise` is stuck behind that call, so the relay session
dies unnoticed while the video threads go on reporting 0 fps and systemd sees a
healthy process. Nothing in-process recovers from a deadlocked client library,
hence a thread of its own that trusts neither the clock nor the loop, and an
exit that skips every cleanup that could hang: systemd restarts the worker and
the next run sweeps whatever this one left in the world.
"""

from __future__ import annotations

import asyncio
import faulthandler
import logging
import os
import sys
import threading
import time
from typing import Callable, NoReturn

log = logging.getLogger(__name__)

# How often the event loop proves it is still turning.
PULSE_INTERVAL = 1.0


class Heartbeat:
    """A timestamp one thread refreshes and the watchdog reads.

    Unarmed until its first beat: a heartbeat nobody has started yet is not
    stale, it is not being watched.
    """

    def __init__(self, name: str, now: Callable[[], float] = time.monotonic) -> None:
        self.name = name
        self._now = now
        self._last: float | None = None

    def beat(self) -> None:
        self._last = self._now()

    def age(self) -> float:
        if self._last is None:
            return 0.0
        return self._now() - self._last


def die(reason: str) -> NoReturn:
    """Exit without unwinding, so nothing that hangs gets a say in the shutdown.

    The stacks are the only record of where the hang was, so they go out first.
    """
    log.critical("%s; dumping every thread and exiting for systemd to restart", reason)
    for handler in logging.getLogger().handlers:
        handler.flush()
    faulthandler.dump_traceback(all_threads=True)
    sys.stderr.flush()
    os._exit(1)


class Watchdog:
    """Fail the process once any heartbeat is older than `stall` seconds.

    Watches its own event loop heartbeat alongside the ones handed in. Those
    are armed at `start`; the loop's is armed by its first `pulse`, so a
    startup spent in blocking CARLA calls before `supervise` runs, each bounded
    by the RPC timeout but unbounded in number, counts against nobody.
    """

    def __init__(self, stall: float, *heartbeats: Heartbeat, now: Callable[[], float] = time.monotonic) -> None:
        self.loop = Heartbeat("event loop", now)
        self._armed = heartbeats
        self._heartbeats = (self.loop, *heartbeats)
        self._stall = stall
        # Check often enough that a stall is caught soon after the threshold,
        # not up to a whole interval late.
        self._interval = min(PULSE_INTERVAL, stall / 4)
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="watchdog", daemon=True)

    def start(self) -> None:
        for heartbeat in self._armed:
            heartbeat.beat()
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=5.0)

    async def pulse(self) -> None:
        """Refresh the event loop heartbeat until cancelled."""
        while True:
            self.loop.beat()
            await asyncio.sleep(self._interval)

    def check(self) -> str | None:
        """Why the process should end, or None while every heartbeat is fresh."""
        for heartbeat in self._heartbeats:
            age = heartbeat.age()
            if age > self._stall:
                return f"{heartbeat.name} has not advanced in {age:.1f} s (limit {self._stall:.1f} s)"
        return None

    def _run(self) -> None:
        while not self._stop.wait(self._interval):
            if (reason := self.check()) is not None:
                die(reason)
