import asyncio
import contextlib
from types import SimpleNamespace

import pytest

from moq_carla_sim import runtime
from moq_carla_sim.drive import Command, Drive, DriveState, Gear, TimestampEntry
from moq_carla_sim.publish import Pose
from moq_carla_sim.runtime import MAX_STAGE_MS, StuckDetector, latency_breakdown


class FakeClock:
    """Records holds so a test can assert what keeps the world stepping."""

    def __init__(self, running: bool = True) -> None:
        self.held: set[str] = set()
        self.running = running

    def hold(self, reason: str) -> None:
        self.held.add(reason)

    def release(self, reason: str) -> None:
        self.held.discard(reason)


class DemandPublisher:
    def __init__(self) -> None:
        self.use = asyncio.Event()
        self.unuse = asyncio.Event()
        self.cuts = 0

    async def used(self) -> None:
        await self.use.wait()
        self.use.clear()

    async def unused(self) -> None:
        await self.unuse.wait()
        self.unuse.clear()

    def cut(self) -> None:
        self.cuts += 1


def test_latency_breakdown_survives_any_finite_timestamps() -> None:
    stages = (
        TimestampEntry(label="received", ts=-1e308),
        TimestampEntry(label="decoded", ts=1e308),
        TimestampEntry(label="rendered", ts=-1e308),
    )

    assert [stage["ms"] for stage in latency_breakdown(stages)] == [MAX_STAGE_MS, 0]


def test_takeover_synchronizes_gate_with_current_vehicle_controls() -> None:
    async def scenario() -> None:
        measured = DriveState(steering=-0.3, throttle=0.7, brake=0.0, gear=Gear.FORWARD)
        commands = asyncio.Queue()
        commands.put_nowait(Command(kind="drive", driver_id="driver", sequence=1, state=measured))

        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(
            name="truck1",
            autonomous=True,
            telemetry=lambda: SimpleNamespace(measured=measured),
        )
        session._drive = Drive()
        session._commands = commands
        session._deadline = None
        session._media_pipeline = {}
        session._actuate = lambda: None
        session._publish = lambda: None

        task = asyncio.create_task(session._loop())
        try:
            for _ in range(10):
                if session._drive.driver is not None:
                    break
                await asyncio.sleep(0)
            assert session._drive.driver == "driver"
            assert session._drive.state == measured
        finally:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

    asyncio.run(scenario())


def test_only_unestopped_lease_holder_can_respawn_truck() -> None:
    async def scenario() -> None:
        commands = asyncio.Queue()
        commands.put_nowait(Command(kind="respawn", driver_id="bystander", sequence=2))
        commands.put_nowait(Command(kind="respawn", driver_id="holder", sequence=3))
        commands.put_nowait(Command(kind="reset_estop", driver_id="holder", sequence=4))
        commands.put_nowait(Command(kind="respawn", driver_id="holder", sequence=5))
        respawns: list[str] = []
        published: list[bool] = []

        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1", respawn=lambda: respawns.append("truck1"))
        session._drive = Drive()
        session._stuck = StuckDetector()
        assert session._drive.apply(Command(kind="drive", driver_id="holder", sequence=1, state=DriveState()))
        assert session._drive.apply(Command(kind="estop", driver_id="bystander", sequence=2))
        session._commands = commands
        session._deadline = None
        session._media_pipeline = {"holder": []}
        session._actuate = lambda: None
        session._publish = lambda: published.append(True)

        task = asyncio.create_task(session._loop())
        try:
            for _ in range(10):
                if respawns:
                    break
                await asyncio.sleep(0)
            assert respawns == ["truck1"]
            assert session._drive.driver is None
            assert session._media_pipeline == {}
            assert published == [True, True]
        finally:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

    asyncio.run(scenario())


def test_video_demand_renders_the_world_for_the_truck(monkeypatch: pytest.MonkeyPatch) -> None:
    """The stream's demand is what turns the simulator's renderer on for a truck."""

    async def wait_forever(*_args: object) -> None:
        await asyncio.Event().wait()

    async def scenario() -> None:
        demanded: list[bool] = []
        watching = asyncio.Event()

        async def watch_demand(on_demand) -> None:
            on_demand(True)
            on_demand(False)
            watching.set()
            await asyncio.Event().wait()

        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1", set_video_demand=demanded.append)
        session._commands = asyncio.Queue()
        session._video = SimpleNamespace(watch_demand=watch_demand, published=[0, 0], rendition_names=["sd", "hd"])
        session._status = DemandPublisher()
        session._publish = lambda: None
        session._loop = wait_forever
        monkeypatch.setattr(runtime, "watch_drivers", wait_forever)

        run = asyncio.create_task(session.run(SimpleNamespace(), "viewer/carla/truck1"))
        await watching.wait()
        run.cancel()
        await asyncio.gather(run, return_exceptions=True)
        assert demanded == [True, False]

    asyncio.run(scenario())


def test_session_exits_when_its_video_demand_fails(monkeypatch: pytest.MonkeyPatch) -> None:
    """A world that cannot start rendering for a viewer fails the session."""

    async def wait_forever(*_args: object) -> None:
        await asyncio.Event().wait()

    async def fail_demand(_on_demand) -> None:
        raise RuntimeError("CARLA rejected settings")

    async def scenario() -> None:
        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1", set_video_demand=lambda _demanded: None)
        session._commands = asyncio.Queue()
        session._video = SimpleNamespace(watch_demand=fail_demand, published=[0], rendition_names=["hd"])
        session._status = DemandPublisher()
        session._publish = lambda: None
        session._loop = wait_forever
        monkeypatch.setattr(runtime, "watch_drivers", wait_forever)

        with pytest.raises(ExceptionGroup) as failure:
            await session.run(SimpleNamespace(), "viewer/carla/truck1")

        assert len(failure.value.exceptions) == 1
        assert str(failure.value.exceptions[0]) == "CARLA rejected settings"

    asyncio.run(scenario())


def test_status_republishes_on_a_timer_so_telemetry_keeps_arriving(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An autonomous truck publishes no accepted-state change, but still drives."""

    async def scenario() -> None:
        published: list[int] = []
        slept: list[float] = []

        async def sleep(delay: float) -> None:
            slept.append(delay)
            if len(slept) == 3:
                raise asyncio.CancelledError

        monkeypatch.setattr(runtime.asyncio, "sleep", sleep)
        session = telemetry_session(pose=lambda: Pose(x=0.0, y=0.0, yaw=0.0, speed=0.0))
        session._publish = lambda _telemetry: published.append(len(published))

        with pytest.raises(asyncio.CancelledError):
            await session._publish_telemetry()

        assert slept == [runtime.TELEMETRY_INTERVAL] * 3
        assert len(published) == 2

    asyncio.run(scenario())


def telemetry_session(pose, *, autonomous: bool = True) -> runtime.TruckSession:
    """A session with just what the telemetry timer touches, at the resting state."""
    session = runtime.TruckSession.__new__(runtime.TruckSession)
    session._clock = FakeClock()
    session._truck = SimpleNamespace(
        name="truck1",
        autonomous=autonomous,
        telemetry=lambda: SimpleNamespace(pose=pose()),
        respawn=lambda: None,
        release=lambda: None,
    )
    session._drive = Drive()
    session._stuck = StuckDetector()
    return session


def test_telemetry_timer_keeps_releasing_a_driverless_truck(monkeypatch: pytest.MonkeyPatch) -> None:
    """The handback merges over several ticks, so the timer must revisit it."""

    async def scenario() -> None:
        ticks = 0
        releases = 0

        async def sleep(_delay: float) -> None:
            nonlocal ticks
            ticks += 1
            if ticks > 5:
                raise asyncio.CancelledError

        def release() -> None:
            nonlocal releases
            releases += 1

        monkeypatch.setattr(runtime.asyncio, "sleep", sleep)
        session = telemetry_session(pose=lambda: Pose(x=0.0, y=0.0, yaw=0.0, speed=8.0), autonomous=False)
        session._truck.release = release
        session._publish = lambda _telemetry: None

        with pytest.raises(asyncio.CancelledError):
            await session._publish_telemetry()
        assert releases == 5

        # A held or e-stopped truck is not driverless, and stays off autonomy.
        for hold in (holding, estopped_by_bystander):
            ticks = releases = 0
            session = telemetry_session(pose=lambda: Pose(x=0.0, y=0.0, yaw=0.0, speed=0.0), autonomous=False)
            session._truck.release = release
            session._publish = lambda _telemetry: None
            hold(session._drive)
            with pytest.raises(asyncio.CancelledError):
                await session._publish_telemetry()
            assert releases == 0

    asyncio.run(scenario())


def test_stuck_detector_fires_only_after_the_timeout_without_displacement() -> None:
    detector = StuckDetector(timeout=300.0, distance=2.0)
    parked = Pose(x=10.0, y=10.0, yaw=0.0, speed=0.0)

    assert not detector.update(parked, now=0.0)
    assert not detector.update(parked, now=299.9)
    # Grinding sideways against a barrier stays inside the window.
    assert not detector.update(Pose(x=11.0, y=10.0, yaw=0.0, speed=0.3), now=299.9)
    assert detector.update(parked, now=300.0)

    # Getting clear re-anchors and restarts the clock.
    assert not detector.update(Pose(x=12.0, y=10.0, yaw=0.0, speed=0.0), now=301.0)
    assert not detector.update(Pose(x=12.0, y=10.0, yaw=0.0, speed=0.0), now=600.0)
    assert detector.update(Pose(x=12.0, y=10.0, yaw=0.0, speed=0.0), now=601.0)


def test_autonomous_truck_wedged_for_the_timeout_is_respawned(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario() -> None:
        clock = 0.0
        respawns: list[float] = []
        published: list[Pose] = []

        async def sleep(delay: float) -> None:
            nonlocal clock
            clock += delay
            if clock > runtime.STUCK_TIMEOUT + 1.0:
                raise asyncio.CancelledError

        monkeypatch.setattr(runtime.asyncio, "sleep", sleep)
        loop = asyncio.get_running_loop()
        monkeypatch.setattr(loop, "time", lambda: clock)

        wedged = Pose(x=5.0, y=5.0, yaw=0.0, speed=0.0)
        session = telemetry_session(pose=lambda: wedged)
        session._truck.respawn = lambda: respawns.append(clock)
        session._publish = lambda telemetry: published.append(telemetry.pose)

        with pytest.raises(asyncio.CancelledError):
            await session._publish_telemetry()

        assert len(respawns) == 1
        assert runtime.STUCK_TIMEOUT <= respawns[0] < runtime.STUCK_TIMEOUT + 1.0
        # Status keeps flowing through the whole wait, and after the respawn.
        assert len(published) > respawns[0] / runtime.TELEMETRY_INTERVAL

    asyncio.run(scenario())


def holding(drive: Drive) -> None:
    """A driver parked with the brake on."""
    assert drive.apply(Command(kind="drive", driver_id="holder", sequence=1, state=DriveState(brake=1.0)))


def estopped_by_bystander(drive: Drive) -> None:
    """A bystander's e-stop outlives the lease it never held."""
    assert drive.apply(Command(kind="estop", driver_id="bystander", sequence=1))
    assert drive.driver is None and drive.estopped


@pytest.mark.parametrize("hold", [holding, estopped_by_bystander])
def test_stuck_watchdog_leaves_a_held_or_estopped_truck_alone(monkeypatch: pytest.MonkeyPatch, hold) -> None:
    async def scenario() -> None:
        clock = 0.0
        respawns: list[float] = []

        async def sleep(delay: float) -> None:
            nonlocal clock
            clock += delay
            if clock > 2 * runtime.STUCK_TIMEOUT:
                raise asyncio.CancelledError

        monkeypatch.setattr(runtime.asyncio, "sleep", sleep)
        loop = asyncio.get_running_loop()
        monkeypatch.setattr(loop, "time", lambda: clock)

        wedged = Pose(x=5.0, y=5.0, yaw=0.0, speed=0.0)
        session = telemetry_session(pose=lambda: wedged, autonomous=False)
        session._truck.respawn = lambda: respawns.append(clock)
        session._publish = lambda _telemetry: None
        hold(session._drive)

        with pytest.raises(asyncio.CancelledError):
            await session._publish_telemetry()

        assert respawns == []

    asyncio.run(scenario())


def test_session_announces_with_the_run_epoch_after_populating_tracks() -> None:
    """The player lists trucks from announcements, and a broadcast starts
    unadvertised, so a session that never announces is invisible. The route
    carries the run's epoch so a restarted worker reads as a restart."""
    calls: list[object] = []
    publication = SimpleNamespace(
        broadcast=SimpleNamespace(
            publish_track=lambda name, info=None: calls.append(name) or SimpleNamespace(demand=SimpleNamespace)
        ),
        announce=lambda route: calls.append(route),
    )
    route = SimpleNamespace(epoch="0190b2a4-0000-7000-8000-000000000000")

    # The video tracks were advertised when the bridge stream opened.
    runtime.TruckSession(SimpleNamespace(name="truck1"), publication, SimpleNamespace(), None, None, FakeClock(), route)

    assert calls == ["status", route]


def test_a_driver_keeps_the_world_stepping_with_nobody_watching() -> None:
    """Video demand and the lease are independent; the gate must hold for both."""

    async def scenario() -> None:
        commands = asyncio.Queue()
        commands.put_nowait(Command(kind="drive", driver_id="holder", sequence=1, state=DriveState(throttle=0.5)))

        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1", autonomous=False)
        session._drive = Drive()
        session._commands = commands
        session._deadline = None
        session._media_pipeline = {}
        session._stuck = StuckDetector()
        session._actuate = lambda: None
        session._publish = lambda: None

        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(session._loop(), timeout=0.1)

        assert session._clock.held == {"drive:truck1"}

        # The watchdog hands the truck back, and the hold goes with the lease.
        session._commands.put_nowait(Command(kind="watchdog_expired"))
        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(session._loop(), timeout=0.1)

        assert session._clock.held == set()

    asyncio.run(scenario())


def test_a_parked_world_is_not_a_stuck_truck(monkeypatch: pytest.MonkeyPatch) -> None:
    """Nothing is meant to be moving, so respawning on it would fire forever."""

    async def scenario() -> None:
        clock = 0.0
        respawns: list[float] = []

        async def sleep(delay: float) -> None:
            nonlocal clock
            clock += delay
            if clock > runtime.STUCK_TIMEOUT * 2:
                raise asyncio.CancelledError

        monkeypatch.setattr(runtime.asyncio, "sleep", sleep)
        loop = asyncio.get_running_loop()
        monkeypatch.setattr(loop, "time", lambda: clock)

        session = telemetry_session(pose=lambda: Pose(x=5.0, y=5.0, yaw=0.0, speed=0.0))
        session._clock = FakeClock(running=False)
        session._truck.respawn = lambda: respawns.append(clock)
        session._publish = lambda _telemetry: None

        with pytest.raises(asyncio.CancelledError):
            await session._publish_telemetry()

        assert respawns == []

    asyncio.run(scenario())


def test_a_status_subscriber_keeps_the_world_stepping() -> None:
    """The map draws positions from `status`, so an open map must not freeze."""

    async def scenario() -> None:
        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1")
        session._status = DemandPublisher()
        watcher = asyncio.create_task(session._watch_status_demand())

        async def expect_held(held: set[str]) -> None:
            for _ in range(10):
                if session._clock.held == held:
                    return
                await asyncio.sleep(0)
            pytest.fail(f"holds never became {held}, still {session._clock.held}")

        try:
            session._status.use.set()
            await expect_held({"status:truck1"})
            session._status.unuse.set()
            await expect_held(set())
            session._status.use.set()
            await expect_held({"status:truck1"})
        finally:
            watcher.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await watcher
        # Cancelled mid-subscription, as a session shutting down would be.
        assert session._clock.held == set()

    asyncio.run(scenario())


def test_an_estopped_holder_neither_steps_the_world_nor_keeps_its_lease(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A latched truck stands still, so its refused frames must let the lease lapse."""

    async def scenario() -> None:
        now = 0.0
        loop = asyncio.get_running_loop()
        monkeypatch.setattr(loop, "time", lambda: now)
        published: list[bool] = []

        session = runtime.TruckSession.__new__(runtime.TruckSession)
        session._clock = FakeClock()
        session._truck = SimpleNamespace(name="truck1", autonomous=False)
        session._drive = Drive()
        session._commands = asyncio.Queue()
        session._deadline = None
        session._media_pipeline = {}
        session._actuate = lambda: None
        session._publish = lambda: published.append(True)

        async def settle(count: int) -> None:
            for _ in range(10):
                if len(published) == count:
                    return
                await asyncio.sleep(0)
            pytest.fail(f"expected {count} publishes, saw {len(published)}")

        task = asyncio.create_task(session._loop())
        try:
            session._commands.put_nowait(
                Command(kind="drive", driver_id="holder", sequence=1, state=DriveState(brake=1.0))
            )
            await settle(1)
            assert session._clock.held == {"drive:truck1"}

            session._commands.put_nowait(Command(kind="estop", driver_id="holder", sequence=2))
            await settle(2)
            assert session._drive.driver == "holder"
            assert session._clock.held == set()
            assert session._deadline == runtime.COMMAND_TIMEOUT

            # The holder keeps sending and the gate refuses every frame. A
            # bystander's e-stop publishes without refreshing the lease, which
            # marks the refused frame as processed.
            now = runtime.COMMAND_TIMEOUT / 2
            session._commands.put_nowait(
                Command(kind="drive", driver_id="holder", sequence=3, state=DriveState(throttle=0.5))
            )
            session._commands.put_nowait(Command(kind="estop", driver_id="bystander", sequence=1))
            await settle(3)
            assert session._deadline == runtime.COMMAND_TIMEOUT
            assert session._clock.held == set()
        finally:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

    asyncio.run(scenario())
