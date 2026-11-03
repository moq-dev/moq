import asyncio
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

    def closed(_payload: object) -> None:
        raise moq.Error("track closed")

    publisher = publish.StatusPublisher.__new__(publish.StatusPublisher)
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
    """The scripted listing of driver paths."""

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_exc) -> None:
        pass

    def __aiter__(self):
        return self

    async def __anext__(self):
        return await self.next()

    async def present(self, path: str) -> None:
        await self.send(moq.AnnounceEventPresent(SimpleNamespace(prefix=path)))

    async def absent(self, path: str) -> None:
        await self.send(moq.AnnounceEventAbsent(SimpleNamespace(prefix=path)))


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

    async def run(self, sequence: int) -> None:
        """Hand out an instance that serves one drive command numbered `sequence`, then stays open."""

        async def control():
            yield {"type": "drive", "sequence": sequence, "throttle": 0.5}
            await asyncio.Event().wait()

        async def subscribe_track(name: str):
            assert name == "control"
            return control()

        await self.send(SimpleNamespace(subscribe_track=subscribe_track))

    async def offline(self) -> None:
        await self.send(None)


class Drivers:
    """A relay holding driver broadcasts."""

    def __init__(self) -> None:
        self.announcements = Announcements()
        self.followed: list[str] = []
        self._followings: dict[str, Following] = {}

    def announced(self, prefix: str, *, filter: str):
        assert (prefix, filter) == ("viewer/carla/truck1", "*")
        return self.announcements

    def following(self, path: str) -> Following:
        return self._followings.setdefault(path, Following())

    def follow(self, path: str) -> Following:
        self.followed.append(path)
        return self.following(path)


async def watching(scenario) -> None:
    """Run `scenario(relay, commands, gate)` against a live `watch_drivers`."""
    relay = Drivers()
    commands: asyncio.Queue = asyncio.Queue()
    watcher = asyncio.create_task(publish.watch_drivers(relay, "viewer/carla/truck1", commands))
    try:
        await scenario(relay, commands, Drive())
    finally:
        watcher.cancel()
        with pytest.raises(asyncio.CancelledError):
            await watcher


async def next_command(commands: asyncio.Queue, gate: Drive):
    command = await asyncio.wait_for(commands.get(), timeout=1)
    gate.apply(command)
    return command


DRIVER = "viewer/carla/truck1/abc"


def test_a_driver_whose_broadcast_goes_absent_loses_the_lease() -> None:
    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present(DRIVER)
        await relay.following(DRIVER).run(1)
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id) == ("drive", "abc")
        assert gate.driver == "abc"

        await relay.announcements.absent(DRIVER)
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", command.instance)
        assert gate.driver is None

    asyncio.run(watching(scenario))


def test_a_driver_whose_run_goes_offline_loses_the_lease_until_it_returns() -> None:
    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present(DRIVER)
        await relay.following(DRIVER).run(1)
        first = await next_command(commands, gate)

        await relay.following(DRIVER).offline()
        left = await next_command(commands, gate)
        assert (left.kind, left.instance) == ("driver_left", first.instance)
        assert gate.driver is None

        await relay.following(DRIVER).run(2)
        second = await next_command(commands, gate)
        assert second.kind == "drive" and second.instance != first.instance
        assert gate.driver == "abc"

    asyncio.run(watching(scenario))


def test_a_refused_driver_is_still_followed() -> None:
    """A refusal leaves the follow waiting, so the driver's next run still takes the lease."""

    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present(DRIVER)
        await relay.following(DRIVER).refuse()
        await relay.following(DRIVER).run(1)
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id) == ("drive", "abc")
        assert relay.followed == [DRIVER]

    asyncio.run(watching(scenario))


def test_a_driver_that_restarts_under_a_new_instance_is_a_new_driver() -> None:
    """The old run's lease ends with it, even though the path and id are the same."""

    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present(DRIVER)
        await relay.following(DRIVER).run(1)
        first = await next_command(commands, gate)

        await relay.following(DRIVER).run(2)
        received = [await next_command(commands, gate) for _ in range(2)]
        assert {(command.kind, command.instance == first.instance) for command in received} == {
            ("driver_left", True),
            ("drive", False),
        }
        # The path stays present across the restart, so one follow sees both runs.
        assert relay.followed == [DRIVER]

        # Whichever order those two arrived in, the new run holds the lease
        # by its next command, and the old run can no longer release it.
        second = next(command.instance for command in received if command.kind == "drive")
        assert not gate.holds(Command(kind="drive", driver_id="abc", instance=first.instance))
        gate.apply(Command(kind="drive", driver_id="abc", instance=second, sequence=3, state=DriveState()))
        assert gate.holds(Command(kind="drive", driver_id="abc", instance=second))
        assert not gate.apply(Command(kind="driver_left", driver_id="abc", instance=first.instance))
        assert gate.driver == "abc"

    asyncio.run(watching(scenario))


def test_only_direct_children_of_the_prefix_are_drivers() -> None:
    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.present("viewer/carla")
        await relay.announcements.present("viewer/carla/truck1/abc/deeper")
        await relay.announcements.present("viewer/carla/truck10")
        assert relay.followed == []

    asyncio.run(watching(scenario))
