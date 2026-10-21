import asyncio

from moq_carla_sim.watchdog import Heartbeat, Watchdog


class FakeTime:
    """A monotonic clock that only moves when the test says so."""

    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


def test_a_stale_heartbeat_ends_the_process() -> None:
    """A clock thread that never returns from `world.tick()` beats nothing."""
    time = FakeTime()
    clock = Heartbeat("simulation clock", time)
    watchdog = Watchdog(0.1, clock, now=time)
    # `start` arms the clock; its thread is stopped at once so the checks below
    # are the only ones, and frozen time gives it nothing to find meanwhile.
    watchdog.start()
    watchdog.stop()

    time.now = 0.1
    assert watchdog.check() is None
    time.now = 0.2
    reason = watchdog.check()
    assert reason is not None and reason.startswith("simulation clock has not advanced")


def test_a_blocked_event_loop_is_a_stall_even_while_the_clock_ticks() -> None:
    """The loop's own heartbeat is watched, since every asyncio watcher is stuck behind a blocked loop."""
    time = FakeTime()
    clock = Heartbeat("simulation clock", time)
    watchdog = Watchdog(0.1, clock, now=time)

    async def scenario() -> str | None:
        pulse = asyncio.create_task(watchdog.pulse())
        try:
            # One turn of the loop for the pulse's first beat.
            await asyncio.sleep(0)
            # A blocking call on the loop thread: nothing awaits, so the pulse
            # cannot run, while the clock keeps beating from its own thread.
            for _ in range(20):
                time.now += 0.01
                clock.beat()
            return watchdog.check()
        finally:
            pulse.cancel()

    reason = asyncio.run(scenario())
    assert reason is not None and reason.startswith("event loop has not advanced")


def test_a_worker_making_progress_is_left_alone() -> None:
    time = FakeTime()
    clock = Heartbeat("simulation clock", time)
    watchdog = Watchdog(0.1, clock, now=time)
    for _ in range(20):
        time.now += 0.025
        clock.beat()
        watchdog.loop.beat()
        assert watchdog.check() is None


def test_the_loop_is_watched_only_once_its_pulse_starts() -> None:
    """Startup runs blocking CARLA calls before `supervise` schedules the pulse; that is not a stall."""
    time = FakeTime()
    clock = Heartbeat("simulation clock", time)
    watchdog = Watchdog(0.05, clock, now=time)
    clock.beat()
    for _ in range(30):
        time.now += 0.01
        clock.beat()
        assert watchdog.check() is None
