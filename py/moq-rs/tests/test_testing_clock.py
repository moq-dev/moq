"""The test clock decides when native time passes."""

import subprocess
import sys
import textwrap
from pathlib import Path


def _run(script: str) -> None:
    # The native runtime is process-global and never leaves the test clock, so
    # each case gets a process of its own. It runs beside `clock.py` to import it.
    subprocess.run([sys.executable, "-c", textwrap.dedent(script)], check=True, cwd=Path(__file__).parent)


def test_a_pending_timer_waits_for_advance() -> None:
    _run(
        """
        import asyncio
        from datetime import timedelta

        import moq
        import pytest
        from clock import Clock

        clock = Clock()

        async def main():
            # The origin's driver runs the timers that judge a group stale.
            origin = moq.OriginProducer()
            route = origin.route("clock", hidden=True)
            track = route.broadcast.publish_track("track", moq.TrackInfo(max_age=None))
            # A range with no start and no delay.min reaches back over every group
            # delay.max keeps; a timed start would be history, which never goes stale.
            consumer = track.consume(moq.Subscription(delay=moq.Delay(max=timedelta(seconds=1))))

            # Group 0 stalls after its first frame, so a read of its next frame waits
            # until group 1 has been out for delay.max: a native timer is pending.
            _stalled = track.append_group(b"stalled", moq.Timestamp.unscaled(0))
            stalled = await asyncio.wait_for(consumer.next_group(), timeout=5)
            assert stalled is not None
            first = await stalled.read_frame()
            assert first is not None and first.payload == b"stalled"
            later = track.append_group(b"later", moq.Timestamp.unscaled(1))
            later.finish()
            pending = asyncio.ensure_future(stalled.read_frame())

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

            # Stale, group 0 is dropped and the read moves on to group 1.
            clock.advance(0.001)
            with pytest.raises(moq.Error):
                await asyncio.wait_for(pending, timeout=5)
            frame = await asyncio.wait_for(consumer.read_frame(), timeout=5)
            assert frame is not None and frame.payload == b"later"
            assert clock.time() == 1

        asyncio.run(main())
        """
    )


def test_clock_refuses_invalid_advances() -> None:
    _run(
        """
        from clock import Clock

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
        from clock import Clock

        moq.BroadcastProducer()
        try:
            Clock()
        except RuntimeError:
            pass
        else:
            raise AssertionError("installed on a runtime already on the host clock")
        """
    )
