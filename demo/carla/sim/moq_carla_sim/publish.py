"""MoQ wiring: video and status out, driver control in.

The broadcast layout is the one `demo/carla/README.md` documents and the web
player speaks:

    carla/{truck}/{camera}.hang              video (sd, hd) + status
    viewer/carla/{truck}/{driverId}          control
"""

from __future__ import annotations

import asyncio
import itertools
import logging
import math
from dataclasses import dataclass
from typing import Any

import moq

from .drive import Command, DriveState, Gear, TimestampEntry

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class Pose:
    """Where a truck is, in CARLA world meters.

    Deliberately the simulator's own top-down axes (x east, y south, yaw
    clockwise from +x) rather than latitude and longitude: the shipped towns
    carry a null OpenDRIVE georeference, so `transform_to_geolocation` returns
    real WGS84 numbers for a meaningless place in the Gulf of Guinea. The road
    map baked into the web player is in these same meters, so the two line up.
    """

    x: float
    y: float
    yaw: float
    speed: float


@dataclass(frozen=True)
class Motion:
    """How the truck actually responded, in its own frame.

    Acceleration in m/s^2 (longitudinal positive forward, lateral positive
    right) and yaw rate in deg/s. Lateral acceleration is the number that rolls
    a loaded truck over, and none of these can be derived from the command:
    they are what the physics did with it.
    """

    yaw_rate: float
    accel_lon: float
    accel_lat: float


@dataclass(frozen=True)
class Collision:
    """The most recent contact the vehicle's collision sensor reported.

    `at` is unix milliseconds at the instant contact began, and is what gives a
    collision an identity on a snapshot track: without one, the retained
    snapshot a late subscriber receives is indistinguishable from a fresh
    impact, and a restarted producer reuses its predecessor's numbering.

    An absolute instant rather than an age, and the distinction is the whole
    design. An age would redirty the snapshot on every publish for a parked
    truck; this stays fixed until the next collision. It also means a viewer
    who opened the page long afterwards ages it correctly, instead of timing it
    from when that particular browser happened to first see it.
    """

    at: float
    other: str
    impulse: float


@dataclass(frozen=True)
class Telemetry:
    """One consistent read of the vehicle: where it is, and how it responded.

    Grouped because they come from a single read of the actor and always travel
    together. The wire keeps them as separate top-level `status` fields, which
    is a contract; this struct is only the plumbing behind it.
    """

    pose: Pose
    measured: DriveState
    motion: Motion
    collision: Collision | None


@dataclass(frozen=True)
class Status:
    accepted: DriveState
    estopped: bool
    estop_by: str | None
    autonomous: bool
    driver: str | None
    sequence: int | None
    media_pipeline: dict[str, list[dict[str, Any]]]
    location: str | None
    map_name: str | None
    telemetry: Telemetry | None


def status_payload(status: Status) -> dict[str, Any]:
    """Build the `status` track payload.

    `mode` is `simulated` because accepted commands actuate a CARLA vehicle;
    the player treats it as an open wire enum, so another value degrades to
    `unknown` instead of blanking the tile. Who is steering is a separate
    question and rides on `autonomy`.

    `accepted` is the command the gate let through; `measured`, `motion`, and
    `collision` are what the vehicle did with it, and are present whatever is
    driving.
    """
    payload: dict[str, Any] = {
        "accepted": {
            "steering": status.accepted.steering,
            "throttle": status.accepted.throttle,
            "brake": status.accepted.brake,
            "gear": status.accepted.gear.value,
        },
        "mode": "simulated",
        "capabilities": ["respawn"],
        "autonomy": "autonomous" if status.autonomous else "manual",
        "estopped": status.estopped,
        "media_pipeline": status.media_pipeline,
    }
    # Omitted rather than null when absent; the player reads a missing field
    # as unset.
    if status.driver is not None:
        payload["driver"] = status.driver
    if status.estop_by is not None:
        payload["estop_by"] = status.estop_by
    if status.sequence is not None:
        payload["sequence"] = status.sequence
    if status.location is not None:
        payload["location"] = status.location
    if status.map_name is not None:
        payload["map"] = status.map_name
    if status.telemetry is not None:
        # Everything below is rounded for the same reason: unrounded float noise
        # from the physics engine would republish a "changed" snapshot for a
        # parked truck forever. Centimetres and tenths of a degree for the pose,
        # a tenth of a percent for a normalized control axis.
        pose = status.telemetry.pose
        payload["pose"] = {
            "x": round(pose.x, 2),
            "y": round(pose.y, 2),
            "yaw": round(pose.yaw, 1),
            "speed": round(pose.speed, 2),
        }
        measured = status.telemetry.measured
        payload["measured"] = {
            "steering": round(measured.steering, 3),
            "throttle": round(measured.throttle, 3),
            "brake": round(measured.brake, 3),
            "gear": measured.gear.value,
        }
        motion = status.telemetry.motion
        payload["motion"] = {
            "yaw_rate": round(motion.yaw_rate, 1),
            "accel_lon": round(motion.accel_lon, 2),
            "accel_lat": round(motion.accel_lat, 2),
        }
        if status.telemetry.collision is not None:
            collision = status.telemetry.collision
            payload["collision"] = {
                "at": round(collision.at),
                "other": collision.other,
                "impulse": round(collision.impulse, 1),
            }
    return payload


class StatusPublisher:
    """The `status` track: JSON snapshots on a compressed track.

    Snapshot rather than stream, because a late joiner wants the truck's current
    state, not the history of every command it ever accepted.
    """

    def __init__(self, broadcast: moq.BroadcastProducer) -> None:
        track = broadcast.publish_track("status", moq.TrackInfo(compress=True))
        self._producer = moq.json.SnapshotProducer(broadcast, track)
        # Weak: watching demand neither keeps the track open nor locks it.
        self._demand = self._producer.demand()

    def publish(self, payload: dict[str, Any]) -> None:
        """Publish the latest status, failing the session if the track is gone.

        Deliberately not swallowed. `update` fails only on a producer that has
        been closed, which never reopens, and status republishes on a 100 ms
        timer, so catching here would retry a dead track ten times a second per
        truck, forever, logging as it went. Status is the control echo, so a
        truck that cannot answer is not one anybody should be steering: let it
        reach the session, exactly as the video pump does with its own track.
        """
        self._producer.update(payload)

    async def used(self) -> None:
        while await self._demand.wanted_changed() is None:
            pass

    async def unused(self) -> None:
        while await self._demand.wanted_changed() is not None:
            pass


# The player's pipeline stages. Anything else a driver sends is dropped, so an
# anonymous lease holder cannot grow the status the truck echoes.
TIMESTAMP_LABELS = ("received", "decoded", "rendered")


def _parse_timestamps(raw: Any) -> tuple[TimestampEntry, ...]:
    if not isinstance(raw, list):
        return ()
    entries: dict[str, TimestampEntry] = {}
    for item in raw:
        if not isinstance(item, dict):
            continue
        label = item.get("label")
        if label not in TIMESTAMP_LABELS or label in entries:
            continue
        timestamp = _number_or_none(item.get("ts"))
        if timestamp is not None:
            entries[label] = TimestampEntry(label=label, ts=timestamp)
    return tuple(entries.values())


def parse_command(driver_id: str, message: Any, instance: str | None = None) -> Command | None:
    """Turn one `control` track message from a driver run into a Command.

    Returns None for anything unrecognized. A driver is an untrusted publisher,
    so a malformed message is dropped here rather than reaching the gate: the
    axes are clamped downstream, but a wrong *shape* must not raise into the
    control loop and take the truck's whole session with it.
    """
    if not isinstance(message, dict):
        return None

    kind = message.get("type")
    sequence = int(_number(message.get("sequence")))
    timestamps = _parse_timestamps(message.get("timestamps"))

    if kind == "drive":
        gear_raw = message.get("gear", Gear.NEUTRAL.value)
        try:
            gear = Gear(gear_raw)
        except ValueError:
            # An unknown gear is not a reason to drop the whole command; neutral
            # is the safe reading of "we do not know what they asked for".
            log.warning("%s: unknown gear %r, treating as neutral", driver_id, gear_raw)
            gear = Gear.NEUTRAL
        return Command(
            kind="drive",
            driver_id=driver_id,
            instance=instance,
            sequence=sequence,
            state=DriveState(
                steering=_number(message.get("steering")),
                throttle=_number(message.get("throttle")),
                brake=_number(message.get("brake")),
                gear=gear,
            ),
            timestamps=timestamps,
        )

    if kind in ("estop", "reset_estop", "respawn"):
        return Command(kind=kind, driver_id=driver_id, instance=instance, sequence=sequence, timestamps=timestamps)

    return None


def _number(value: Any) -> float:
    """A JSON number, or 0.0 for anything else.

    `bool` is excluded explicitly because it is an `int` subclass in Python, so
    a driver sending `true` would otherwise read as 1.
    """
    return _number_or_none(value) or 0.0


def _number_or_none(value: Any) -> float | None:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    number = float(value)
    return number if math.isfinite(number) else None


async def watch_drivers(
    client: moq.Client,
    prefix: str,
    commands: asyncio.Queue[Command],
) -> None:
    """Discover driver broadcasts at `{prefix}/{driverId}` and fan their commands in.

    Runs until cancelled. Each driver path the listing reports present gets its own
    task, so one slow or stuck driver cannot hold up the others. That task follows
    the path across the driver's runs, one publisher instance each; a run that
    another takes over, or whose path goes absent, ends, and with it its lease.
    """
    drivers: dict[str, asyncio.Task[None]] = {}

    try:
        async with client.announced(prefix, filter="*") as announced:
            async for event in announced:
                # Both variants carry `announce`; uniffi's stubs type only the enum.
                path = event.announce.prefix  # pyright: ignore[reportAttributeAccessIssue]
                driver_id = path.removeprefix(prefix + "/")
                # The filter admits one segment below the prefix; anything else
                # is a route that only overlaps it, not a driver's broadcast.
                if driver_id == path or not driver_id or "/" in driver_id:
                    continue
                if isinstance(event, moq.AnnounceEventAbsent):
                    task = drivers.pop(driver_id, None)
                    if task is not None:
                        task.cancel()
                    continue
                drivers[driver_id] = asyncio.create_task(
                    _follow_driver(driver_id, client, path, commands),
                    name=f"driver:{driver_id}",
                )
    finally:
        for task in drivers.values():
            task.cancel()


# Names each driver run for the lease. The bindings hand out a run's broadcast
# without its route instance, and a process-wide count keeps a run's late
# `driver_left` from releasing a later run of the same driver.
_runs = itertools.count(1)


async def _follow_driver(
    driver_id: str,
    client: moq.Client,
    path: str,
    commands: asyncio.Queue[Command],
) -> None:
    """Pump each run of one driver in turn, ending a run once another takes over or it goes offline."""
    run: asyncio.Task[None] | None = None
    try:
        async with client.follow(path) as following:
            while True:
                try:
                    broadcast = await following.available()
                # A refusal leaves the follow waiting for another run; only shutdown ends it.
                except moq.Error as err:  # pyright: ignore[reportGeneralTypeIssues]
                    if moq.is_shutdown(err):
                        raise
                    log.info("driver %s refused: %s", driver_id, err)
                    continue
                if run is not None:
                    run.cancel()
                    run = None
                if broadcast is None:
                    continue
                instance = str(next(_runs))
                run = asyncio.create_task(
                    _pump_driver(driver_id, instance, broadcast, commands),
                    name=f"driver:{driver_id}:{instance}",
                )
    # uniffi's stubs declare the error a plain class; at runtime it is an Exception.
    except moq.Error as err:  # pyright: ignore[reportGeneralTypeIssues]
        log.info("driver %s follow ended: %s", driver_id, err)
    finally:
        if run is not None:
            run.cancel()


async def _pump_driver(
    driver_id: str,
    instance: str,
    broadcast: moq.BroadcastConsumer,
    commands: asyncio.Queue[Command],
) -> None:
    """Feed one driver run's `control` track into the command queue.

    Always emits `driver_left` on the way out, however it leaves, so the gate
    releases a lease even when the track ends abnormally.
    """
    log.info("driver %s connected (run %s)", driver_id, instance)
    try:
        control = moq.json.SnapshotConsumer(await broadcast.subscribe_track("control"))
        async for message in control:
            command = parse_command(driver_id, message.value, instance)
            if command is not None:
                await commands.put(command)
    # uniffi's stubs declare the error a plain class; at runtime it is an Exception.
    except moq.Error as err:  # pyright: ignore[reportGeneralTypeIssues]
        log.info("driver %s track ended: %s", driver_id, err)
    finally:
        log.info("driver %s disconnected (run %s)", driver_id, instance)
        await commands.put(Command(kind="driver_left", driver_id=driver_id, instance=instance))


__all__ = [
    "StatusPublisher",
    "Collision",
    "Motion",
    "Pose",
    "Status",
    "Telemetry",
    "parse_command",
    "status_payload",
    "watch_drivers",
]
