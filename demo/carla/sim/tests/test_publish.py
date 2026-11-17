import asyncio
import json
from types import SimpleNamespace

import moq
import pytest

from moq_carla_sim import publish
from moq_carla_sim.drive import Command, Drive, DriveState, Gear, TimestampEntry
from moq_carla_sim.publish import (
    Collision,
    Motion,
    Pose,
    Status,
    Telemetry,
    parse_command,
    status_payload,
)


def status(**overrides) -> Status:
    """A status with nothing set, so each test names only what it is about."""
    defaults = dict(
        accepted=DriveState.stopped(),
        estopped=False,
        estop_by=None,
        autonomous=False,
        driver=None,
        sequence=None,
        media_pipeline={},
        location=None,
        map_name=None,
        telemetry=None,
    )
    return Status(**{**defaults, **overrides})


def telemetry(**overrides) -> Telemetry:
    defaults = dict(
        pose=Pose(x=0.0, y=0.0, yaw=0.0, speed=0.0),
        measured=DriveState.stopped(),
        motion=Motion(yaw_rate=0.0, accel_lon=0.0, accel_lat=0.0),
        collision=None,
    )
    return Telemetry(**{**defaults, **overrides})


def test_malformed_timestamp_does_not_drop_driver_command() -> None:
    command = parse_command(
        "driver",
        {
            "type": "drive",
            "sequence": 7,
            "throttle": 0.5,
            "timestamps": [
                {"label": "bad-null", "ts": None},
                {"label": "bad-string", "ts": "12"},
                {"label": "received", "ts": 12.5},
            ],
        },
    )

    assert command is not None
    assert command.sequence == 7
    assert command.timestamps == (TimestampEntry(label="received", ts=12.5),)


def test_an_integer_past_the_float_range_is_not_a_number() -> None:
    command = parse_command("driver", {"type": "drive", "sequence": 10**400, "throttle": 10**400})

    assert command is not None
    assert command.sequence == 0
    assert command.state is not None
    assert command.state.throttle == 0.0


def test_only_the_players_pipeline_stages_are_kept() -> None:
    stages = [{"label": "received", "ts": 1.0}, {"label": "decoded", "ts": 2.0}]
    junk = [{"label": "x" * 10_000, "ts": 3.0}] * 1_000
    command = parse_command(
        "driver",
        {"type": "drive", "sequence": 1, "timestamps": [*stages, *junk, {"label": "received", "ts": 9.0}]},
    )

    assert command is not None
    assert command.timestamps == (
        TimestampEntry(label="received", ts=1.0),
        TimestampEntry(label="decoded", ts=2.0),
    )


def test_respawn_command_is_carla_only_control_input() -> None:
    command = parse_command("driver", {"type": "respawn", "sequence": 8})

    assert command is not None
    assert command.kind == "respawn"
    assert command.driver_id == "driver"
    assert command.sequence == 8


def test_status_names_the_driver_that_latched_estop() -> None:
    payload = status_payload(status(estopped=True, estop_by="bystander", driver="holder", sequence=4))

    assert payload["driver"] == "holder"
    assert payload["estop_by"] == "bystander"
    assert payload["mode"] == "simulated"
    assert payload["capabilities"] == ["respawn"]


def test_status_carries_the_map_and_a_rounded_pose() -> None:
    payload = status_payload(
        status(
            autonomous=True,
            map_name="Town10HD_Opt",
            telemetry=telemetry(pose=Pose(x=12.3456, y=-78.9012, yaw=180.06, speed=9.87654)),
        )
    )

    assert payload["map"] == "Town10HD_Opt"
    assert payload["pose"] == {"x": 12.35, "y": -78.9, "yaw": 180.1, "speed": 9.88}


def test_status_reports_measured_feedback_beside_the_accepted_command() -> None:
    """The pair is the point: what was asked for, and what the vehicle did."""
    payload = status_payload(
        status(
            accepted=DriveState(steering=0.5, throttle=0.4, brake=0.0, gear=Gear.FORWARD),
            telemetry=telemetry(
                measured=DriveState(steering=0.31234, throttle=0.38765, brake=0.0, gear=Gear.FORWARD),
                motion=Motion(yaw_rate=4.567, accel_lon=1.2345, accel_lat=-2.3456),
            ),
        )
    )

    assert payload["accepted"]["steering"] == 0.5
    assert payload["measured"] == {
        "steering": 0.312,
        "throttle": 0.388,
        "brake": 0.0,
        "gear": "forward",
    }
    assert payload["motion"] == {"yaw_rate": 4.6, "accel_lon": 1.23, "accel_lat": -2.35}


def test_status_carries_a_collision_until_a_later_one_replaces_it() -> None:
    """An absolute instant is what makes an event visible on a snapshot track."""
    payload = status_payload(
        status(
            telemetry=telemetry(collision=Collision(at=1756334400123.4, other="vehicle.tesla.model3", impulse=4210.55))
        )
    )

    assert payload["collision"] == {
        "at": 1756334400123,
        "other": "vehicle.tesla.model3",
        "impulse": 4210.6,
    }


def test_status_omits_everything_a_producer_has_none_of() -> None:
    payload = status_payload(status())

    for absent in ("map", "pose", "measured", "motion", "collision"):
        assert absent not in payload


def test_status_omits_only_the_collision_a_truck_has_not_had() -> None:
    payload = status_payload(status(telemetry=telemetry()))

    assert "collision" not in payload
    assert "measured" in payload
    assert "motion" in payload


def test_a_closed_status_track_fails_the_session() -> None:
    """Status republishes on a timer, so a swallowed failure would spin forever."""

    def closed(_payload: object, _timestamp: object) -> None:
        raise moq.Error("track closed")

    publisher = publish.StatusPublisher.__new__(publish.StatusPublisher)
    publisher._catalog = SimpleNamespace(now=lambda: SimpleNamespace(value=0, scale=1_000))
    publisher._producer = SimpleNamespace(update=closed)

    with pytest.raises(moq.Error):
        publisher.publish({})


class Scripted:
    """A scripted async source. `send` returns once the reader has handled the
    item and come back for the next one."""

    def __init__(self) -> None:
        self._items: asyncio.Queue = asyncio.Queue()
        self._idle = asyncio.Event()

    async def next(self):
        if self._items.empty():
            self._idle.set()
        item = await self._items.get()
        self._idle.clear()
        return item

    async def send(self, item) -> None:
        self._idle.clear()
        self._items.put_nowait(item)
        await self._idle.wait()


class Announcements(Scripted):
    """The scripted listing of driver paths, by the id the filter captured."""

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_exc) -> None:
        pass

    def __aiter__(self):
        return self

    async def __anext__(self):
        return await self.next()

    async def present(self, driver: str) -> None:
        await self.send(moq.AnnounceEventPresent(SimpleNamespace(captures=[driver])))

    async def absent(self, driver: str) -> None:
        await self.send(moq.AnnounceEventAbsent(SimpleNamespace(captures=[driver])))

    async def offline(self, driver: str) -> None:
        await self.send(moq.AnnounceEventOffline(SimpleNamespace(captures=[driver])))


class Run:
    """One driver run's broadcast: its `control` track hands out what `send` queues."""

    def __init__(self) -> None:
        self.subscription = None
        self._frames: asyncio.Queue = asyncio.Queue()

    def send(self, message) -> None:
        payload = message if isinstance(message, bytes) else json.dumps(message).encode()
        self._frames.put_nowait(SimpleNamespace(payload=payload))

    def end(self, error: BaseException | None = None) -> None:
        self._frames.put_nowait(error)

    async def subscribe_track(self, name: str, subscription):
        assert name == "control"
        self.subscription = subscription
        return self

    async def read_frame(self):
        frame = await self._frames.get()
        if isinstance(frame, BaseException):
            raise frame
        return frame


class Following(Scripted):
    """A scripted follow of one path: each `run` hands out another instance, `offline` a `None`,
    and `refuse` an error."""

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_exc) -> None:
        pass

    async def available(self):
        item = await self.next()
        if isinstance(item, BaseException):
            raise item
        return item

    async def refuse(self) -> None:
        """Fail the next `available()`, as a handler refusing the path does."""
        await self.send(moq.Error("refused"))

    async def run(self, *messages) -> Run:
        """Hand out an instance whose control track holds `messages`, then stays open."""
        run = Run()
        for message in messages:
            run.send(message)
        await self.send(run)
        return run

    async def offline(self) -> None:
        await self.send(None)


class Relay:
    """A relay holding driver broadcasts."""

    def __init__(self) -> None:
        self.announcements = Announcements()
        self.followed: list[str] = []
        self._followings: dict[str, Following] = {}

    def announced(self, prefix: str, *, filter: str):
        assert (prefix, filter) == (PREFIX, "*")
        return self.announcements

    def following(self, driver: str) -> Following:
        return self._followings.setdefault(f"{PREFIX}/{driver}", Following())

    def follow(self, path: str) -> Following:
        self.followed.append(path)
        return self._followings.setdefault(path, Following())


PREFIX = "viewer/carla/truck1"
DRIVE = {"type": "drive", "sequence": 1, "throttle": 0.5}


async def watching(scenario) -> None:
    """Run `scenario(relay, drivers, commands, gate)` against a live `Drivers`."""
    relay = Relay()
    commands: asyncio.Queue = asyncio.Queue()
    drivers = publish.Drivers(commands)
    task = asyncio.create_task(drivers.run(relay, PREFIX))
    # Raced against the watcher, so a failed one fails the test rather than leaving it waiting.
    play = asyncio.create_task(scenario(relay, drivers, commands, Drive()))
    try:
        await asyncio.wait({task, play}, return_when=asyncio.FIRST_COMPLETED)
        # The watcher never returns on its own, so one that ended failed, whichever finished first.
        if task.done():
            task.result()
            raise AssertionError("the driver watcher stopped before the scenario finished")
        play.result()
    finally:
        play.cancel()
        if not task.done():
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task


async def next_command(commands: asyncio.Queue, gate: Drive):
    command = await commands.get()
    gate.apply(command)
    return command


def test_a_driver_whose_broadcast_goes_absent_loses_the_lease() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").run(DRIVE)
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id) == ("drive", "abc")
        assert gate.driver == "abc"

        await relay.announcements.absent("abc")
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", command.instance)
        assert gate.driver is None

    asyncio.run(watching(scenario))


def test_a_driver_this_side_lets_go_of_loses_the_lease() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").run(DRIVE)
        await next_command(commands, gate)

        await relay.announcements.offline("abc")
        assert (await next_command(commands, gate)).kind == "driver_left"
        assert gate.driver is None

        # Back again, it is followed afresh.
        await relay.announcements.present("abc")
        await relay.following("abc").run(DRIVE)
        assert (await next_command(commands, gate)).kind == "drive"
        assert relay.followed == [f"{PREFIX}/abc"] * 2

    asyncio.run(watching(scenario))


def test_a_driver_whose_run_goes_offline_loses_the_lease_until_it_returns() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").run(DRIVE)
        first = await next_command(commands, gate)

        await relay.following("abc").offline()
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", first.instance)
        assert gate.driver is None

        await relay.following("abc").run(DRIVE)
        second = await next_command(commands, gate)
        assert second.kind == "drive" and second.instance != first.instance
        assert gate.driver == "abc"

    asyncio.run(watching(scenario))


def test_a_failed_control_track_still_releases_its_lease() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        run = await relay.following("abc").run(DRIVE)
        command = await next_command(commands, gate)

        run.end(moq.Error("reset"))
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", command.instance)
        assert gate.driver is None

        # The run's own departure is its only one, however the follow ends it later.
        await relay.following("abc").offline()
        assert commands.empty()

    asyncio.run(watching(scenario))


def test_availability_changes_never_follow_a_driver_twice() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.announcements.send(moq.AnnounceEventPresent(SimpleNamespace(captures=["abc"])))
        assert relay.followed == [f"{PREFIX}/abc"]

    asyncio.run(watching(scenario))


def test_a_refused_driver_is_still_followed() -> None:
    """A refusal leaves the follow waiting, so the driver's next run still takes the lease."""

    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").refuse()
        await relay.following("abc").run(DRIVE)
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id) == ("drive", "abc")
        assert relay.followed == [f"{PREFIX}/abc"]

    asyncio.run(watching(scenario))


def test_a_driver_that_restarts_under_a_new_instance_is_a_new_driver() -> None:
    """The old run's lease ends with it, even though the path and id are the same."""

    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").run(DRIVE)
        first = await next_command(commands, gate)

        await relay.following("abc").run({**DRIVE, "sequence": 2})
        # The old run's departure comes first, after everything it queued.
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", first.instance)
        second = await next_command(commands, gate)
        assert second.kind == "drive" and second.instance != first.instance
        # The path stays present across the restart, so one follow sees both runs.
        assert relay.followed == [f"{PREFIX}/abc"]

        assert gate.holds(second)
        assert not gate.apply(Command(kind="driver_left", driver_id="abc", instance=first.instance))
        assert gate.driver == "abc"

    asyncio.run(watching(scenario))


def test_a_malformed_control_is_dropped_and_the_run_keeps_going() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        await relay.following("abc").run(b"\xff not json", ["not", "a", "command"], DRIVE)
        command = await next_command(commands, gate)
        assert command.kind == "drive"
        assert commands.empty()

    asyncio.run(watching(scenario))


def test_a_late_control_never_blocks_a_newer_one() -> None:
    """The truck reads oldest first, skipping a control one heartbeat behind the newest,
    so a reset sent right before a drive still arrives whole."""

    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        run = await relay.following("abc").run({"type": "reset_estop", "sequence": 1}, {**DRIVE, "sequence": 2})
        assert [(await commands.get()).kind for _ in range(2)] == ["reset_estop", "drive"]
        assert run.subscription == moq.Subscription(
            delay=moq.Delay(max=publish.CONTROL_DELAY), order=moq.Order.ASCENDING
        )

    asyncio.run(watching(scenario))


def test_a_revoked_run_ends_and_nothing_it_sends_reaches_the_gate() -> None:
    async def scenario(relay: Relay, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("abc")
        run = await relay.following("abc").run(DRIVE)
        claim = await next_command(commands, gate)
        assert not drivers.revoked(claim)

        # A heartbeat already queued ahead of the revoke.
        run.send(DRIVE)
        queued = await commands.get()
        drivers.revoke(*gate.holder)
        assert drivers.revoked(queued)

        # One still in flight stops the pump, which reports the departure last.
        run.send(DRIVE)
        left = await commands.get()
        assert (left.kind, left.instance) == ("driver_left", claim.instance)
        assert drivers.revoked(left)
        # Reading the departure forgets the run.
        assert not drivers.revoked(left)

    asyncio.run(watching(scenario))


def test_shutdown_never_waits_on_a_full_command_queue() -> None:
    """Shutdown stops the gate first, so a run cancelled then must not wait to report its departure."""

    async def scenario() -> None:
        relay = Relay()
        commands: asyncio.Queue = asyncio.Queue(maxsize=1)
        task = asyncio.create_task(publish.Drivers(commands).run(relay, PREFIX))
        await relay.announcements.present("abc")
        # The queue fills and the second control waits for room.
        await relay.following("abc").run(DRIVE, DRIVE)
        for _ in range(10):
            await asyncio.sleep(0)
        assert commands.full()

        task.cancel()
        await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), timeout=1)
        assert task.cancelled()

    asyncio.run(scenario())
