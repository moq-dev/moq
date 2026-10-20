import asyncio
import time

from moq_carla_sim.watchdog import Heartbeat, Watchdog


def test_a_stale_heartbeat_ends_the_process() -> None:
    """A clock thread that never returns from `world.tick()` beats nothing."""

    async def scenario() -> list[str]:
        failures: list[str] = []
        clock = Heartbeat("simulation clock")
        watchdog = Watchdog(0.1, clock, fail=failures.append)
        watchdog.start()
        pulse = asyncio.create_task(watchdog.pulse())
        try:
            deadline = time.monotonic() + 2.0
            while not failures and time.monotonic() < deadline:
                await asyncio.sleep(0.01)
        finally:
            pulse.cancel()
            watchdog.stop()
        return failures

    failures = asyncio.run(scenario())
    assert failures and failures[0].startswith("simulation clock has not advanced")


def test_a_blocked_event_loop_is_a_stall_even_while_the_clock_ticks() -> None:
    """The loop's own heartbeat is watched, since every asyncio watcher is stuck behind a blocked loop."""

    async def scenario() -> list[str]:
        failures: list[str] = []
        clock = Heartbeat("simulation clock")
        watchdog = Watchdog(0.1, clock, fail=failures.append)
        watchdog.start()
        pulse = asyncio.create_task(watchdog.pulse())
        try:
            await asyncio.sleep(0.05)
            # A blocking call on the loop thread: nothing awaits, so the pulse
            # cannot run, while the clock keeps beating from its own thread.
            deadline = time.monotonic() + 0.5
            while time.monotonic() < deadline:
                clock.beat()
                time.sleep(0.01)
        finally:
            pulse.cancel()
            watchdog.stop()
        return failures

    failures = asyncio.run(scenario())
    assert failures and failures[0].startswith("event loop has not advanced")


def test_a_worker_making_progress_is_left_alone() -> None:
    async def scenario() -> list[str]:
        failures: list[str] = []
        clock = Heartbeat("simulation clock")
        watchdog = Watchdog(0.1, clock, fail=failures.append)
        watchdog.start()
        pulse = asyncio.create_task(watchdog.pulse())
        try:
            for _ in range(20):
                clock.beat()
                await asyncio.sleep(0.025)
        finally:
            pulse.cancel()
            watchdog.stop()
        return failures

    assert asyncio.run(scenario()) == []


def test_the_loop_is_watched_only_once_its_pulse_starts() -> None:
    """Startup runs blocking CARLA calls before `supervise` schedules the pulse; that is not a stall."""
    failures: list[str] = []
    clock = Heartbeat("simulation clock")
    watchdog = Watchdog(0.05, clock, fail=failures.append)
    watchdog.start()
    try:
        deadline = time.monotonic() + 0.3
        while time.monotonic() < deadline:
            clock.beat()
            time.sleep(0.01)
    finally:
        watchdog.stop()
    assert failures == []
