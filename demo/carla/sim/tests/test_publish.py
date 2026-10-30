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


class Announcements:
    """A scripted announcement stream. `send` returns once the watcher has
    handled the event and come back for the next one."""

    def __init__(self) -> None:
        self._events: asyncio.Queue = asyncio.Queue()
        self._idle = asyncio.Event()

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_exc) -> None:
        pass

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._events.empty():
            self._idle.set()
        event = await self._events.get()
        self._idle.clear()
        return event

    async def send(self, kind: type, path: str, epoch: str | None) -> None:
        self._idle.clear()
        self._events.put_nowait(kind(SimpleNamespace(prefix=path, route=SimpleNamespace(epoch=epoch))))
        await self._idle.wait()


class Drivers:
    """A relay holding driver broadcasts: each request serves one drive command
    numbered by how many requests came before it, then stays open."""

    def __init__(self) -> None:
        self.announcements = Announcements()
        self.requested: list[tuple[str, str | None]] = []

    def announced(self, prefix: str, *, filter: str):
        assert (prefix, filter) == ("viewer/carla/truck1", "*")
        return self.announcements

    async def request(self, path: str, *, epoch: str | None = None):
        self.requested.append((path, epoch))
        sequence = len(self.requested)

        async def control():
            yield {"type": "drive", "sequence": sequence, "throttle": 0.5}
            await asyncio.Event().wait()

        async def subscribe_track(name: str):
            assert name == "control"
            return control()

        return SimpleNamespace(subscribe_track=subscribe_track)


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


def test_a_driver_run_keeps_the_lease_across_route_updates() -> None:
    """An update is the same run re-priced: no second subscription, no release."""

    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla/truck1/abc", "e1")
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id, command.epoch) == ("drive", "abc", "e1")
        assert gate.driver == "abc"

        await relay.announcements.send(moq.AnnounceEventUpdate, "viewer/carla/truck1/abc", "e1")
        assert commands.empty()
        assert gate.driver == "abc"
        assert relay.requested == [("viewer/carla/truck1/abc", "e1")]

    asyncio.run(watching(scenario))


def test_a_driver_whose_broadcast_ends_loses_the_lease() -> None:
    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla/truck1/abc", "e1")
        await next_command(commands, gate)
        assert gate.driver == "abc"

        await relay.announcements.send(moq.AnnounceEventEnd, "viewer/carla/truck1/abc", "e1")
        left = await next_command(commands, gate)
        assert (left.kind, left.epoch) == ("driver_left", "e1")
        assert gate.driver is None

    asyncio.run(watching(scenario))


@pytest.mark.parametrize("kind", ["restart", "update"])
def test_a_driver_that_restarts_under_a_new_epoch_is_a_new_driver(kind: str) -> None:
    """The old run's lease ends with it, even though the path and id are the same.

    A new epoch is a restart whether the relay says so or reports it as an update.
    """
    event = moq.AnnounceEventRestart if kind == "restart" else moq.AnnounceEventUpdate

    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla/truck1/abc", "e1")
        await next_command(commands, gate)

        await relay.announcements.send(event, "viewer/carla/truck1/abc", "e2")
        received = [await next_command(commands, gate) for _ in range(2)]
        assert {(command.kind, command.epoch) for command in received} == {("driver_left", "e1"), ("drive", "e2")}
        assert relay.requested == [("viewer/carla/truck1/abc", "e1"), ("viewer/carla/truck1/abc", "e2")]

        # Whichever order those two arrived in, the new run holds the lease
        # by its next command, and the old run can no longer release it.
        assert not gate.holds(Command(kind="drive", driver_id="abc", epoch="e1"))
        gate.apply(Command(kind="drive", driver_id="abc", epoch="e2", sequence=3, state=DriveState()))
        assert gate.holds(Command(kind="drive", driver_id="abc", epoch="e2"))
        assert not gate.apply(Command(kind="driver_left", driver_id="abc", epoch="e1"))
        assert gate.driver == "abc"

    asyncio.run(watching(scenario))


def test_only_direct_children_of_the_prefix_are_drivers() -> None:
    async def scenario(relay: Drivers, commands: asyncio.Queue, gate: Drive) -> None:
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla", None)
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla/truck1/abc/deeper", None)
        await relay.announcements.send(moq.AnnounceEventStart, "viewer/carla/truck10", None)
        assert relay.requested == []

    asyncio.run(watching(scenario))
