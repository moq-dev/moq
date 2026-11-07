import asyncio
import json
from types import SimpleNamespace

import moq
import pytest

from moq_carla_sim import publish
from moq_carla_sim.drive import Command, Drive, DriveState, Gear, TimestampEntry, handle
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
        "s1",
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
        "s1",
    )

    assert command is not None
    assert command.timestamps == (
        TimestampEntry(label="received", ts=1.0),
        TimestampEntry(label="decoded", ts=2.0),
    )


def test_respawn_command_is_carla_only_control_input() -> None:
    command = parse_command("driver", {"type": "respawn", "sequence": 8}, "s1")

    assert command is not None
    assert command.kind == "respawn"
    assert command.driver_id == "driver"
    assert command.sequence == 8


def test_status_names_the_driver_that_latched_estop() -> None:
    payload = status_payload(status(estopped=True, estop_by="bystander", driver="holder"))

    assert payload["driver"] == "holder"
    assert payload["estop_by"] == "bystander"
    assert payload["mode"] == "simulated"
    assert payload["capabilities"] == ["respawn"]
    # Acknowledgements are the holder's alone, on its own session.
    assert "sequence" not in payload
    assert "media_pipeline" not in payload


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


class Session:
    """One accepted driver session: controls the driver sends, replies the truck writes."""

    def __init__(self, id: str) -> None:
        self.session = id
        self.consumer = self
        self.producer = self
        self.subscription = None
        self.replies: list[dict] = []
        self.finished = False
        self._frames: asyncio.Queue = asyncio.Queue()

    def send(self, message) -> None:
        payload = message if isinstance(message, bytes) else json.dumps(message).encode()
        self._frames.put_nowait(SimpleNamespace(payload=payload))

    def end(self, error: BaseException | None = None) -> None:
        self._frames.put_nowait(error)

    def update(self, subscription) -> None:
        self.subscription = subscription

    async def read_frame(self):
        frame = await self._frames.get()
        if isinstance(frame, BaseException):
            raise frame
        return frame

    def write_frame(self, payload: bytes) -> None:
        if self.finished:
            raise moq.Error("session closed")
        self.replies.append(json.loads(payload))

    def finish(self) -> None:
        self.finished = True


class Incoming:
    """A dial claiming `peer`; accepting it fails the return-routability check when forged."""

    def __init__(self, peer: str, session: Session | None) -> None:
        self.peer = peer
        self._session = session

    async def accept(self) -> Session:
        if self._session is None:
            raise moq.Error("not dialed")
        return self._session


class Endpoint:
    """A truck's endpoint, handing out scripted dials."""

    def __init__(self) -> None:
        self.dials: asyncio.Queue = asyncio.Queue()
        self.opened: list[tuple[str, str]] = []
        self.closed = False

    def __call__(self, origin, prefix: str, id: str) -> "Endpoint":
        self.opened.append((prefix, id))
        return self

    async def __aenter__(self) -> "Endpoint":
        return self

    async def __aexit__(self, *_exc) -> None:
        self.closed = True

    async def accept_track(self) -> Incoming:
        return await self.dials.get()

    def dial(self, peer: str, id: str | None) -> Session | None:
        session = None if id is None else Session(id)
        self.dials.put_nowait(Incoming(peer, session))
        return session


async def serving(scenario, monkeypatch: pytest.MonkeyPatch) -> None:
    """Run `scenario(endpoint, drivers, commands, gate)` against a live `Drivers`."""
    endpoint = Endpoint()
    monkeypatch.setattr(moq.socket, "Endpoint", endpoint, raising=False)
    commands: asyncio.Queue = asyncio.Queue()
    drivers = publish.Drivers("truck1", commands)
    task = asyncio.create_task(drivers.run(SimpleNamespace()))
    try:
        await scenario(endpoint, drivers, commands, Drive())
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    assert endpoint.opened == [("carla", "truck1")]
    assert endpoint.closed


async def next_command(commands: asyncio.Queue, gate: Drive):
    command = await asyncio.wait_for(commands.get(), timeout=1)
    gate.apply(command)
    return command


DRIVE = {"type": "drive", "sequence": 1, "throttle": 0.5}


def test_a_driver_whose_session_ends_loses_the_lease(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        session = endpoint.dial("driver-abc", "s1")
        session.send(DRIVE)
        command = await next_command(commands, gate)
        assert (command.kind, command.driver_id, command.session) == ("drive", "driver-abc", "s1")
        assert gate.driver == handle("s1")

        session.end()
        left = await next_command(commands, gate)
        assert (left.kind, left.session) == ("driver_left", "s1")
        assert gate.driver is None
        assert session.finished

    asyncio.run(serving(scenario, monkeypatch))


def test_a_failed_session_still_releases_its_lease(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        session = endpoint.dial("driver-abc", "s1")
        session.send(DRIVE)
        await next_command(commands, gate)

        session.end(moq.Error("reset"))
        left = await next_command(commands, gate)
        assert (left.kind, left.session) == ("driver_left", "s1")
        assert gate.driver is None

    asyncio.run(serving(scenario, monkeypatch))


def test_shutdown_never_waits_on_a_full_command_queue(monkeypatch: pytest.MonkeyPatch) -> None:
    """Shutdown stops the gate first, so a pump cancelled then must not wait to report its departure."""

    async def scenario() -> None:
        endpoint = Endpoint()
        monkeypatch.setattr(moq.socket, "Endpoint", endpoint, raising=False)
        commands: asyncio.Queue = asyncio.Queue(maxsize=1)
        drivers = publish.Drivers("truck1", commands)
        task = asyncio.create_task(drivers.run(SimpleNamespace()))
        session = endpoint.dial("driver-abc", "s1")
        session.send(DRIVE)
        session.send(DRIVE)
        # The queue is full and the second control is waiting for room.
        for _ in range(10):
            await asyncio.sleep(0)
        assert commands.full()

        task.cancel()
        await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), timeout=1)
        assert session.finished

    asyncio.run(scenario())


def test_a_new_session_of_the_same_driver_is_a_new_driver(monkeypatch: pytest.MonkeyPatch) -> None:
    """A lease is one session: an endpoint posing under the holder's id never continues it."""

    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        first = endpoint.dial("driver-abc", "s1")
        first.send(DRIVE)
        await next_command(commands, gate)

        second = endpoint.dial("driver-abc", "s2")
        second.send({**DRIVE, "throttle": 1.0})
        command = await next_command(commands, gate)
        assert command.session == "s2"
        assert not gate.holds(command)

        second.end()
        assert (await next_command(commands, gate)).kind == "driver_left"
        assert gate.driver == handle("s1")
        assert gate.holds(Command(kind="drive", driver_id="driver-abc", session="s1"))

    asyncio.run(serving(scenario, monkeypatch))


def test_a_second_session_under_an_open_driver_and_session_id_is_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        first = endpoint.dial("driver-abc", "s1")
        first.send(DRIVE)
        await next_command(commands, gate)

        copy = endpoint.dial("driver-abc", "s1")
        copy.send({**DRIVE, "throttle": 1.0})
        first.send({**DRIVE, "sequence": 2})
        command = await next_command(commands, gate)
        assert command.sequence == 2
        assert copy.finished
        assert commands.empty()
        drivers.reply(command, {"sequence": 2})
        assert first.replies == [{"sequence": 2}]

    asyncio.run(serving(scenario, monkeypatch))


def test_a_forged_dial_never_reaches_the_gate(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        endpoint.dial("driver-forged", None)
        session = endpoint.dial("driver-abc", "s1")
        session.send(DRIVE)
        command = await next_command(commands, gate)
        assert command.driver_id == "driver-abc"
        assert commands.empty()

    asyncio.run(serving(scenario, monkeypatch))


def test_a_malformed_control_is_dropped_and_the_session_keeps_going(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        session = endpoint.dial("driver-abc", "s1")
        session.send(b"\xff not json")
        session.send(["not", "a", "command"])
        session.send(DRIVE)
        command = await next_command(commands, gate)
        assert command.kind == "drive"

    asyncio.run(serving(scenario, monkeypatch))


def test_a_late_control_never_blocks_a_newer_one(monkeypatch: pytest.MonkeyPatch) -> None:
    """The truck reads oldest first, skipping a control one heartbeat behind the newest."""

    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        session = endpoint.dial("driver-abc", "s1")
        session.send(DRIVE)
        await next_command(commands, gate)
        assert session.subscription == moq.Subscription(
            delay=moq.Delay(max=publish.CONTROL_DELAY), order=moq.Order.ASCENDING
        )

    asyncio.run(serving(scenario, monkeypatch))


def test_replies_reach_only_their_own_open_session(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario(endpoint: Endpoint, drivers, commands: asyncio.Queue, gate: Drive) -> None:
        first = endpoint.dial("driver-abc", "s1")
        second = endpoint.dial("driver-def", "s2")
        first.send(DRIVE)
        second.send(DRIVE)
        await next_command(commands, gate)
        await next_command(commands, gate)

        drivers.reply(Command(kind="drive", driver_id="driver-abc", session="s1"), {"sequence": 1})
        assert first.replies == [{"sequence": 1}]
        assert second.replies == []
        # A session id is the dialer's pick, so only the pair names a session.
        drivers.reply(Command(kind="drive", driver_id="driver-def", session="s1"), {"sequence": 9})
        assert first.replies == [{"sequence": 1}]
        assert second.replies == []

        first.end()
        await next_command(commands, gate)
        # The pump releasing the lease may race a reply to the gone session.
        drivers.reply(Command(kind="drive", driver_id="driver-abc", session="s1"), {"sequence": 2})
        assert first.replies == [{"sequence": 1}]

    asyncio.run(serving(scenario, monkeypatch))
