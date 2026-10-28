"""The per-truck control loop.

One `TruckSession` per truck: it owns the safety gate, the MoQ broadcast, and
the truck's bridge stream, whose compressed video the bridge publishes into
that broadcast.
"""

from __future__ import annotations

import asyncio
import logging
import math
from typing import TYPE_CHECKING, Any

import moq

from .drive import Accepted, Command, Drive, TimestampEntry
from .publish import Pose, Status, StatusPublisher, Telemetry, status_payload, watch_drivers
from .world import drive_hold, status_hold

if TYPE_CHECKING:
    from .bridge import BridgeStream
    from .world import Clock, Truck

log = logging.getLogger(__name__)

# How long the truck waits for the lease holder before releasing the lease.
# The web player heartbeats at 250 ms.
COMMAND_TIMEOUT = 1.0

# How often a session reports its published frame rate.
REPORT_INTERVAL = 10.0

# How often status republishes the vehicle's own telemetry, so map markers keep
# moving and measured feedback keeps arriving while nobody is driving.
TELEMETRY_INTERVAL = 0.1

# Pending driver commands held before publishers are made to wait.
COMMAND_QUEUE_DEPTH = 64

# An autonomous truck that has not moved this far in this long is wedged, not
# waiting: Traffic Manager would have deleted it, which is what the hero role
# forbids, so this is where a stuck truck gets unstuck. Town10's lights cycle in
# well under a minute, and the trucks queued behind a wedged one respawn with it
# rather than sit there once it is gone. Displacement rather than speed, because
# a truck grinding against a barrier reports velocity while going nowhere.
STUCK_TIMEOUT = 300.0
STUCK_DISTANCE = 2.0


class StuckDetector:
    """Notices a truck that has stopped going anywhere.

    `update` anchors on the first pose it sees and reports stuck once the truck
    has stayed within `distance` of that anchor for `timeout`. Moving that far
    re-anchors and restarts the clock.
    """

    def __init__(self, timeout: float = STUCK_TIMEOUT, distance: float = STUCK_DISTANCE) -> None:
        self._timeout = timeout
        self._distance = distance
        self._anchor: Pose | None = None
        self._since = 0.0

    def reset(self) -> None:
        self._anchor = None

    def update(self, pose: Pose, now: float) -> bool:
        anchor = self._anchor
        if anchor is None or math.hypot(pose.x - anchor.x, pose.y - anchor.y) >= self._distance:
            self._anchor = pose
            self._since = now
            return False
        return now - self._since >= self._timeout


# Longer than any real pipeline stage. A driver's timestamps are any finite
# floats, whose difference can be infinite, so it is clamped before `int`.
MAX_STAGE_MS = 60_000


def latency_breakdown(timestamps: tuple[TimestampEntry, ...]) -> list[dict[str, Any]]:
    """Per-stage deltas from the driver's media timestamps, in milliseconds."""
    return [
        {"label": f"{a.label}→{b.label}", "ms": int(min(max(b.ts - a.ts, 0), MAX_STAGE_MS))}
        for a, b in zip(timestamps, timestamps[1:])
    ]


class TruckSession:
    """One truck's gate, broadcast, and control loop."""

    def __init__(
        self,
        truck: Truck,
        publication: moq.Publication,
        video: BridgeStream,
        location: str | None,
        map_name: str | None,
        clock: Clock,
        route: moq.AnnounceRoute,
    ) -> None:
        self._truck = truck
        self._video = video
        self._clock = clock
        self._location = location
        self._map_name = map_name
        self._drive = Drive()
        self._status = StatusPublisher(publication.broadcast)
        # Bounded: drivers are untrusted publishers, so an unbounded queue is a
        # memory-growth path under one that floods; a full queue instead blocks
        # that driver's own task.
        self._commands: asyncio.Queue[Command] = asyncio.Queue(maxsize=COMMAND_QUEUE_DEPTH)
        self._media_pipeline: dict[str, list[dict[str, Any]]] = {}
        self._deadline: float | None = None
        self._stuck = StuckDetector()
        # A broadcast starts unadvertised; the player discovers trucks by
        # announcement, so advertise once the status and video tracks exist.
        # The stream's are there already: opening it advertised them. The
        # route carries the run's epoch, so viewers see a restarted worker as
        # a restart rather than a stalled broadcast.
        publication.announce(route)

    def stop_video(self) -> None:
        """End the video tracks cleanly and stop the bridge encoding them."""
        self._video.finish()

    @property
    def commands(self) -> asyncio.Queue[Command]:
        return self._commands

    async def run(self, client: moq.Client, drivers: str) -> None:
        """Consume driver commands and actuate until cancelled.

        Driver discovery runs in a task group rather than a bare task, so a
        failure there fails the session. A detached task's exception is never
        retrieved: the truck would keep publishing video and status while
        silently accepting no driver ever again, which reads as healthy.
        """
        self._publish()
        async with asyncio.TaskGroup() as group:
            watcher = group.create_task(
                watch_drivers(client, drivers, self._commands), name=f"drivers:{self._truck.name}"
            )
            # Video demand is what renders the world for this truck, and the
            # bridge follows the same demand to pick its camera.
            demand = group.create_task(
                self._video.watch_demand(self._truck.set_video_demand), name=f"demand:{self._truck.name}"
            )
            video = group.create_task(self._report_video(), name=f"video:{self._truck.name}")
            telemetry = group.create_task(self._publish_telemetry(), name=f"telemetry:{self._truck.name}")
            viewers = group.create_task(self._watch_status_demand(), name=f"viewers:{self._truck.name}")
            try:
                await self._loop()
            finally:
                # Before the tasks, so a truck on its way out cannot leave the
                # world stepping for a driver it no longer has.
                self._clock.release(drive_hold(self._truck.name))
                watcher.cancel()
                video.cancel()
                demand.cancel()
                telemetry.cancel()
                viewers.cancel()

    async def _loop(self) -> None:
        while True:
            command = await self._next_command()
            if command.kind == "respawn":
                # Respawn changes the simulated actor rather than the shared
                # drive contract. Only the lease holder may move the truck.
                if not self._drive.holds(command) or self._drive.estopped:
                    continue
                self._truck.respawn()
                self._drive = Drive()
                self._deadline = None
                self._media_pipeline.clear()
                self._stuck.reset()
                self._hold_clock()
                self._publish()
                continue
            if command.kind == "drive" and self._drive.driver is None and self._truck.autonomous:
                # The browser seeds its first command from the latest status, but
                # the gate still holds its no-driver resting state. Synchronize the
                # gear from fresher local feedback so the shift interlock does not
                # turn a moving takeover into a neutral coast.
                self._drive.synchronize(self._truck.telemetry().measured)
            accepted = self._drive.apply(command)
            changed = accepted
            self._hold_clock()

            if command.kind in ("drive", "estop", "reset_estop"):
                if accepted and self._drive.holds(command):
                    # Only the lease holder's accepted traffic refreshes the
                    # watchdog and its pipeline timing. A bystander's e-stop
                    # still latches, but must not extend the holder's lease, and
                    # an e-stopped holder's refused drive frames must not keep
                    # the lease alive forever.
                    self._media_pipeline[command.driver_id] = latency_breakdown(command.timestamps)
                    self._deadline = asyncio.get_running_loop().time() + COMMAND_TIMEOUT
            elif command.kind == "driver_left" and accepted:
                # Only the holder's run has a pipeline entry, so another run of
                # the same driver id leaving must not drop it.
                self._media_pipeline.pop(command.driver_id, None)
                self._deadline = None
            elif command.kind == "watchdog_expired" and accepted:
                self._deadline = None
                self._media_pipeline.clear()
                log.warning("%s: control watchdog expired, returning to autonomy", self._truck.name)

            if not changed:
                continue

            # Actuate before publishing, so the world and the status echo can
            # never disagree about what was accepted.
            self._actuate()
            self._publish()

    async def _report_video(self) -> None:
        """Log the published frame rate per rendition.

        A truck whose simulator cannot keep up still publishes, just at a lower
        rate, so nothing else would say so.
        """
        loop = asyncio.get_running_loop()
        published = list(self._video.published)
        since = loop.time()
        while True:
            await asyncio.sleep(REPORT_INTERVAL)
            now = loop.time()
            current = list(self._video.published)
            rates = [(count - before) / (now - since) for count, before in zip(current, published)]
            if any(rates):
                log.info(
                    "%s: publishing %s",
                    self._truck.name,
                    ", ".join(f"{name}={rate:.1f} fps" for name, rate in zip(self._video.rendition_names, rates)),
                )
            published, since = current, now

    async def _publish_telemetry(self) -> None:
        """Republish on a timer so a truck nobody is driving still reports.

        The drive loop publishes only when the gate's accepted state changes,
        and the vehicle's own telemetry moves independently of it: without this
        an autonomous truck would pin its map marker wherever the last driver
        let go of it, and report the pedals it was applying at that moment.

        Unbounded on purpose: a publish that fails raises out of this task and
        into the session's task group rather than being retried, so the timer
        cannot outlive the track it publishes to.

        The stuck watchdog rides on the same tick. It only ever fires for a
        truck nobody holds: a stationary truck under a driver is the driver's
        business, and an e-stopped one is latched on purpose. So does the
        handback: a released truck steers itself onto a lane before Traffic
        Manager takes it, and nothing else runs often enough to drive it.
        """
        while True:
            await asyncio.sleep(TELEMETRY_INTERVAL)
            telemetry = self._truck.telemetry()
            if self._drive.driver is None and not self._drive.estopped:
                self._truck.release()
            if (
                self._drive.driver is not None
                or self._drive.estopped
                or not self._truck.autonomous
                or not self._clock.running
            ):
                # A parked world is not a stuck truck: nothing is meant to be
                # moving, and respawning on that would fire every STUCK_TIMEOUT
                # for as long as nobody watches.
                self._stuck.reset()
            elif self._stuck.update(telemetry.pose, asyncio.get_running_loop().time()):
                log.warning("%s: no movement in %.0f s while autonomous, respawning", self._truck.name, STUCK_TIMEOUT)
                self._truck.respawn()
                self._stuck.reset()
            self._publish(telemetry)

    async def _next_command(self) -> Command:
        """The next driver command, or a synthesized watchdog expiry."""
        if self._deadline is None:
            return await self._commands.get()

        timeout = self._deadline - asyncio.get_running_loop().time()
        if timeout <= 0:
            return Command(kind="watchdog_expired")
        try:
            return await asyncio.wait_for(self._commands.get(), timeout=timeout)
        except asyncio.TimeoutError:
            return Command(kind="watchdog_expired")

    async def _watch_status_demand(self) -> None:
        """Step the world while anyone subscribes to this truck's status.

        The map draws every truck from `status`, not video, so parking on video
        demand alone froze every marker under an open map. The release is in a
        `finally` so a cancelled watcher cannot leave the world stepping.
        """
        reason = status_hold(self._truck.name)
        while True:
            await self._status.used()
            self._clock.hold(reason)
            try:
                await self._status.unused()
            finally:
                self._clock.release(reason)

    def _hold_clock(self) -> None:
        """Step the world while this truck has a driver, whoever is watching.

        Video demand parks the clock, and the two do not have to agree: a driver
        whose viewer has gone would otherwise steer a frozen world, with every
        command accepted and nothing moving. An e-stop is deliberately not a
        hold, even under a driver. It latches until reset, so holding for it
        would keep the world stepping indefinitely for a truck that is meant to
        be standing still.
        """
        reason = drive_hold(self._truck.name)
        if self._drive.driver is not None and not self._drive.estopped:
            self._clock.hold(reason)
        else:
            self._clock.release(reason)

    def _actuate(self) -> None:
        self._truck.actuate(self._drive.accepted(), has_driver=self._drive.driver is not None)

    def _publish(self, telemetry: Telemetry | None = None) -> None:
        self._status.publish(
            status_payload(
                Status(
                    accepted=self._drive.state,
                    estopped=self._drive.estopped,
                    estop_by=self._drive.estop_by,
                    autonomous=self._truck.autonomous,
                    driver=self._drive.driver,
                    sequence=self._drive.sequence,
                    media_pipeline=self._media_pipeline,
                    location=self._location,
                    map_name=self._map_name,
                    telemetry=self._truck.telemetry() if telemetry is None else telemetry,
                )
            )
        )


__all__ = [
    "COMMAND_TIMEOUT",
    "STUCK_DISTANCE",
    "STUCK_TIMEOUT",
    "TELEMETRY_INTERVAL",
    "Accepted",
    "StuckDetector",
    "TruckSession",
    "latency_breakdown",
]
