"""The test clock decides when native time passes."""

import subprocess
import sys
import textwrap


def _run(script: str) -> None:
    # The native runtime is process-global and never leaves the test clock, so
    # each case gets a process of its own.
    subprocess.run([sys.executable, "-c", textwrap.dedent(script)], check=True)


def test_a_pending_timer_waits_for_advance() -> None:
    _run(
        """
        import asyncio
        from datetime import timedelta

        import moq
        from moq.testing import Clock

        clock = Clock()

        async def main():
            # The origin's driver runs the timers that judge a group stale.
            origin = moq.OriginProducer()
            track = origin.create_broadcast("clock").broadcast.publish_track("track")
            from_group_0 = [moq.Range(start=moq.Anchor.ABSOLUTE(group=0))]
            consumer = track.consume(moq.Subscription(max_delay=timedelta(seconds=1), ranges=from_group_0))

            # Group 0 stalls with no frame, so the in-order read waits on it until
            # group 1 has been out for max_delay: a native timer is pending.
            _stalled = track.append_group()
            later = track.append_group()
            later.write_frame(b"later")
            later.finish()
            pending = asyncio.ensure_future(consumer.read_frame())

            # Real time passing is the stimulus here, not a synchronization: paused
            # tokio would jump to the timer within this window, while the test clock
            # must not move. The deadline is absolute (group 1's arrival plus 1s), so
            # it holds whether or not the driver armed it yet; a loaded host can only
            # make these checks pass vacuously, never fail.
            await asyncio.sleep(0.05)
            assert not pending.done()
            assert clock.time() == 0

            clock.advance(0.999)
            await asyncio.sleep(0.05)
            assert not pending.done()

            clock.advance(0.001)
            frame = await asyncio.wait_for(pending, timeout=5)
            assert frame is not None and frame.payload == b"later"
            assert clock.time() == 1

        asyncio.run(main())
        """
    )


def test_clock_refuses_invalid_advances() -> None:
    _run(
        """
        from moq.testing import Clock

        clock = Clock()
        for seconds in (-1, float("nan"), float("inf")):
            try:
                clock.advance(seconds)
            except ValueError:
                pass
            else:
                raise AssertionError(f"advance({seconds}) accepted")
        assert clock.time() == 0

        # Past the wall clock's range, which starts in 2025: refused, not a native panic.
        try:
            clock.advance(17_000_000_000)
        except OverflowError:
            pass
        else:
            raise AssertionError("advance past the clock's range accepted")
        assert clock.time() == 0
        """
    )


def test_clock_refuses_a_started_runtime() -> None:
    _run(
        """
        import moq
        from moq.testing import Clock

        moq.BroadcastProducer()
        try:
            Clock()
        except RuntimeError:
            pass
        else:
            raise AssertionError("installed on a runtime already on the host clock")
        """
    )
