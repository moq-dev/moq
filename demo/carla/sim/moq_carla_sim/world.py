"""The CARLA side: one world, several trucks, demand-gated cameras.

Cameras never hand pixels to this process. Each feeds the GPU video bridge
inside the simulator (`demo/carla/bridge`), which renders them only while the
worker's stream wants video; see `bridge`.

A single client process owns the world. CARLA advises one command-sending
client, and one owner keeps Traffic Manager state coherent, so every truck lives
here rather than in a process each. That is also the point of the demo: they
share a world and can see each other.

Autonomy is the resting state. A truck nobody is driving runs on Traffic
Manager. Takeover adopts its latest measured controls so switching to manual
does not change steering or pedal demand underneath the driver.
"""

from __future__ import annotations

import asyncio
import logging
import math
import threading
import time
from dataclasses import dataclass
from typing import Callable

import carla

from . import handback
from .drive import Accepted, DriveState, Gear
from .publish import Collision, Motion, Pose, Telemetry
from .watchdog import Heartbeat

log = logging.getLogger(__name__)


def connect_client(host: str, port: int, timeout: float) -> carla.Client:
    """Connect to the simulator, refusing a client the server cannot speak to.

    Fail here rather than at the first spawn: a version mismatch produces
    confusing missing-blueprint errors much further along.
    """
    client = carla.Client(host, port)
    client.set_timeout(timeout)
    server = client.get_server_version()
    local = client.get_client_version()
    if server != local:
        raise RuntimeError(
            f"CARLA version mismatch: server {server}, client {local}. "
            "The python wheel and the server image must match."
        )
    log.info("connected to CARLA %s", server)
    return client


# CARLA's HGV truck, shipped from 0.9.15 on.
TRUCK_BLUEPRINT = "vehicle.carlamotors.european_hgv"

# Cab-height forward camera. The longitudinal position comes from each
# blueprint's bounding box: the HGV's origin is near its center, so a small
# fixed x offset would leave the sensor inside the cab.
CAMERA_HEIGHT = 2.8
CAMERA_STANDOFF = 0.25
CONVOY_SPACING = 12.0

# The preview rendition every truck marker on the map plays at once.
SD_HEIGHT = 240

# The camera attribute naming the bridge stream a camera renders for. Only the
# patched simulator build has it (https://github.com/moq-dev/carla).
BRIDGE_ATTRIBUTE = "moq_bridge_stream"

# How often the clock reports its tick durations, matching the video pump's
# throughput line so the two can be read together.
CLOCK_REPORT_INTERVAL = 10.0

# How long a spawn waits for the tick that lands its actor. The clock ticks at
# the frame period, so anything near this means it is not running.
SPAWN_TICK_TIMEOUT = 5.0

# CARLA reports a collision event for every frame contact persists, so a truck
# resting against a barrier would otherwise report thirty collisions a second.
# Continuing contact with the SAME actor inside this window is coalesced into
# the one collision it is, keeping its peak impulse. Striking something else is
# a new collision however soon it follows: scraping a barrier and then hitting a
# truck half a second later is two events, and folding the second into the first
# would hide it behind the first one's count and actor.
COLLISION_WINDOW = 1.0


def bridge_stream(truck: str, camera: str) -> str:
    """The bridge stream a truck's cameras render for, e.g. `truck1/front`."""
    return f"{truck}/{camera}"


def camera_transform(vehicle: carla.Vehicle) -> carla.Transform:
    box = vehicle.bounding_box
    front = box.location.x + box.extent.x
    return carla.Transform(carla.Location(x=front + CAMERA_STANDOFF, z=CAMERA_HEIGHT))


def nearby_spawn_points(points: list[carla.Transform], origin: carla.Location) -> list[carla.Transform]:
    """Order map spawn points from nearest to farthest from `origin`."""
    return sorted(
        points,
        key=lambda point: (
            (point.location.x - origin.x) ** 2 + (point.location.y - origin.y) ** 2 + (point.location.z - origin.z) ** 2
        ),
    )


def free_spawn_point(points: list[carla.Transform], vehicles: list[carla.Vehicle], own_id: int) -> carla.Transform:
    """Return the first candidate with a truck-length gap from every other vehicle."""

    def separated(vehicle: carla.Vehicle, point: carla.Transform) -> bool:
        if vehicle.id == own_id:
            return True
        location = vehicle.get_transform().location
        distance = (
            (location.x - point.location.x) ** 2
            + (location.y - point.location.y) ** 2
            + (location.z - point.location.z) ** 2
        )
        return distance >= CONVOY_SPACING**2

    for point in points:
        if all(separated(vehicle, point) for vehicle in vehicles):
            return point
    raise RuntimeError(f"every one of the {len(points)} respawn points is occupied")


def _waypoint_key(waypoint: carla.Waypoint) -> tuple[int, int, int, float]:
    return waypoint.road_id, waypoint.section_id, waypoint.lane_id, waypoint.s


def convoy_transform(road_map: carla.Map, lead: carla.Transform, position: int) -> carla.Transform:
    """A road-aligned spawn `position` truck lengths behind the lead."""
    lead_waypoint = road_map.get_waypoint(lead.location)
    choices = [
        waypoint
        for waypoint in lead_waypoint.previous(CONVOY_SPACING * position)
        if waypoint.road_id == lead_waypoint.road_id and waypoint.lane_id == lead_waypoint.lane_id
    ]
    if not choices:
        raise RuntimeError("the lead truck is too close to a junction for the convoy")
    return min(choices, key=_waypoint_key).transform


def convoy_anchor_points(
    road_map: carla.Map,
    points: list[carla.Transform],
    followers: int,
) -> list[carla.Transform]:
    """Spawn points with enough uninterrupted same-lane road behind them."""
    if followers == 0:
        return points
    anchors = []
    for point in points:
        try:
            convoy_transform(road_map, point, followers)
        except RuntimeError:
            continue
        anchors.append(point)
    if not anchors:
        raise RuntimeError(f"the map has no spawn point with room for {followers + 1} trucks")
    return anchors


@dataclass(frozen=True)
class CameraConfig:
    """The encoded video ladder.

    The sd rendition is capped at `SD_HEIGHT` pixels high while preserving the
    camera's aspect ratio. Both dimensions stay even because the encoder's
    4:2:0 planes require it. Bitrates are explicit because the encoder's resolution-derived
    defaults are tuned for efficiency, while this teleoperation demo needs road
    detail to survive motion, and every truck's sd plays at once on the
    map, so this is the rendition that is always on screen.
    """

    width: int = 1280
    height: int = 720
    fps: int = 30
    sd_bitrate: int = 800_000
    hd_bitrate: int = 4_000_000

    def renditions(self) -> tuple[RenditionConfig, ...]:
        scale = min(1.0, SD_HEIGHT / self.height)
        sd_width = max(2, int(self.width * scale) // 2 * 2)
        sd_height = max(2, int(self.height * scale) // 2 * 2)
        return (
            RenditionConfig(
                name="sd",
                width=sd_width,
                height=sd_height,
                fps=self.fps,
                bitrate=self.sd_bitrate,
            ),
            RenditionConfig(
                name="hd",
                width=self.width,
                height=self.height,
                fps=self.fps,
                bitrate=self.hd_bitrate,
            ),
        )


@dataclass(frozen=True)
class RenditionConfig:
    """One named camera rendition."""

    name: str
    width: int
    height: int
    fps: int
    bitrate: int


class Truck:
    """One vehicle plus its cameras, and the autonomy/manual switch between them.

    The cameras, one per rendition size, feed the bridge rather than this
    process, and the bridge picks which of them renders. What the truck
    controls is whether the shared world renders at all for it.
    """

    def __init__(
        self,
        name: str,
        vehicle: carla.Vehicle,
        cameras: list[carla.Sensor],
        collision: carla.Sensor,
        traffic_manager_port: int,
        render_demand: Callable[[str, bool], None] | None = None,
        *,
        road_map: carla.Map,
    ) -> None:
        self.name = name
        self._vehicle = vehicle
        self._road_map = road_map
        self._cameras = cameras
        self._video = False
        self._render_demand = render_demand
        self._tm_port = traffic_manager_port
        self._autonomous = False
        self._spawn_transform = vehicle.get_transform()

        # Unlike the cameras, the collision sensor listens for the truck's whole
        # life rather than following viewer demand. It emits only on contact, so
        # it costs nothing while nothing happens, and the one thing an operator
        # must not miss is a truck nobody was watching hitting something.
        self._collision_sensor = collision
        self._collision_lock = threading.Lock()
        self._collision: Collision | None = None
        self._collision_at = 0.0
        self._collision_actor: int | None = None
        collision.listen(self._on_collision)

        self.engage_autonomy()

    def set_video_demand(self, demanded: bool) -> None:
        """Render the shared world for this truck while anyone wants its video.

        CARLA captures nothing in no-rendering mode, so the stream turns this on
        before telling the bridge and off after.
        """
        if demanded == self._video:
            return
        if self._render_demand is not None:
            self._render_demand(self.name, demanded)
        self._video = demanded
        log.info("%s: video %s", self.name, "wanted" if demanded else "parked")

    def _on_collision(self, event: carla.CollisionEvent) -> None:
        """Record contact, on CARLA's sensor thread.

        Continuing contact with the same actor inside `COLLISION_WINDOW` is
        coalesced into one collision carrying its peak impulse, and keeps
        extending the window, so grinding along a barrier stays one event.
        Anything else struck is a new collision immediately.

        Keyed on the actor's id rather than its `type_id`, which is a blueprint
        name: two of the same model of car are one type and two collisions.
        """
        impulse = event.normal_impulse
        magnitude = math.sqrt(impulse.x**2 + impulse.y**2 + impulse.z**2)
        actor = event.other_actor
        # Monotonic for the coalescing window, which is a duration and must not
        # move when the host clock is stepped; wall clock for the wire, which is
        # an instant a browser on another machine has to age.
        now = time.monotonic()
        with self._collision_lock:
            previous = self._collision
            if (
                previous is not None
                and self._collision_actor == actor.id
                and now - self._collision_at < COLLISION_WINDOW
            ):
                # The timestamp stays at the start of contact: it identifies the
                # collision, so moving it would republish one event as many.
                self._collision = Collision(
                    at=previous.at,
                    other=previous.other,
                    impulse=max(previous.impulse, magnitude),
                )
            else:
                self._collision = Collision(
                    at=time.time() * 1000.0,
                    other=actor.type_id,
                    impulse=magnitude,
                )
                log.warning("%s: collision with %s", self.name, self._collision.other)
            self._collision_actor = actor.id
            self._collision_at = now

    def telemetry(self) -> Telemetry:
        """One consistent read of the vehicle: where it is, and how it responded.

        Every getter here reads the state the client received on the last tick
        rather than issuing an RPC, so polling every truck at the status rate
        costs the simulator nothing.

        `get_control` is the control the physics actually applied, which is
        feedback rather than the command echo, and is populated whoever is
        driving - including Traffic Manager. Acceleration and yaw rate come out
        of the world frame and are projected onto the vehicle's own axes, since
        what matters to a heavy truck is how hard it is being pushed sideways.
        """
        transform = self._vehicle.get_transform()
        velocity = self._vehicle.get_velocity()
        control = self._vehicle.get_control()
        acceleration = self._vehicle.get_acceleration()
        angular = self._vehicle.get_angular_velocity()
        # CARLA's own basis vectors rather than trigonometry on yaw: the world is
        # left-handed with y pointing south, so hand-rolling the rotation is an
        # easy way to publish lateral acceleration with the wrong sign.
        forward = transform.get_forward_vector()
        right = transform.get_right_vector()

        def project(vector: carla.Vector3D, axis: carla.Vector3D) -> float:
            """Component of `vector` along a unit `axis`, in all three dimensions.

            The z terms are not optional padding: pitch tilts `forward` out of
            the horizontal plane and roll tilts `right`, so dropping them
            underreports exactly on the graded, banked roads where the
            numbers matter.
            """
            return vector.x * axis.x + vector.y * axis.y + vector.z * axis.z

        with self._collision_lock:
            collision = self._collision
        return Telemetry(
            pose=Pose(
                x=transform.location.x,
                y=transform.location.y,
                yaw=transform.rotation.yaw,
                speed=math.sqrt(velocity.x**2 + velocity.y**2 + velocity.z**2),
            ),
            measured=DriveState(
                steering=control.steer,
                throttle=control.throttle,
                brake=control.brake,
                # CARLA has no neutral - `actuate` expresses it as zero throttle -
                # so the measured gear is the direction the physics drove in.
                gear=Gear.REVERSE if control.reverse else Gear.FORWARD,
            ),
            motion=Motion(
                yaw_rate=angular.z,
                accel_lon=project(acceleration, forward),
                accel_lat=project(acceleration, right),
            ),
            collision=collision,
        )

    def engage_autonomy(self) -> None:
        """Hand the truck to Traffic Manager where it stands. Idempotent."""
        if self._autonomous:
            return
        self._vehicle.set_autopilot(True, self._tm_port)
        self._autonomous = True
        log.info("%s: autonomy engaged", self.name)

    def release(self) -> None:
        """Return a driverless truck to autonomy, merging onto a lane first.

        Idempotent: the drive loop calls this when the driver lets go, and the
        telemetry timer keeps calling it until the truck is autonomous again.
        Until then the truck coasts under `handback`'s steering; see that
        module for why Traffic Manager cannot simply be given a moving truck.
        """
        if self._autonomous:
            return
        measured = self._vehicle.get_control()
        control = handback.plan(
            self._road_map,
            self._vehicle.get_transform(),
            self._vehicle.get_velocity(),
            measured.steer,
            reversing=measured.reverse,
        )
        if control is None:
            self.engage_autonomy()
        else:
            self._vehicle.apply_control(control)

    def respawn(self) -> None:
        """Return this actor to a free spawn point without breaking its sensors."""
        world = self._vehicle.get_world()
        map_points = self._road_map.get_spawn_points()
        candidates = [
            self._spawn_transform,
            *nearby_spawn_points(map_points, self._spawn_transform.location),
        ]
        transform = free_spawn_point(
            candidates,
            list(world.get_actors().filter("vehicle.*")),
            self._vehicle.id,
        )
        if self._autonomous:
            self._vehicle.set_autopilot(False, self._tm_port)
            self._autonomous = False
        self._vehicle.apply_control(carla.VehicleControl())
        self._vehicle.set_transform(transform)
        self._vehicle.set_target_velocity(carla.Vector3D())
        self._vehicle.set_target_angular_velocity(carla.Vector3D())
        with self._collision_lock:
            self._collision = None
            self._collision_actor = None
            self._collision_at = 0.0
        self.engage_autonomy()
        log.info("%s: respawned", self.name)

    @property
    def autonomous(self) -> bool:
        return self._autonomous

    def _disengage_autonomy(self) -> None:
        if not self._autonomous:
            return
        # Traffic Manager releases the vehicle where it is, still carrying its
        # velocity. The takeover command preserves its latest measured controls.
        self._vehicle.set_autopilot(False, self._tm_port)
        self._autonomous = False
        log.info("%s: autonomy disengaged, driver has control", self.name)

    def actuate(self, accepted: Accepted, *, has_driver: bool) -> None:
        """Apply accepted state to the vehicle, or return it to autonomy.

        Only the gate's accepted state reaches CARLA. The three cases are
        deliberately different, because they are not equally urgent:

        - E-stop is the one hard latch: full brake, autonomy off, and it stays
          that way until an explicit reset.
        - No driver means autonomy resumes, once the truck is somewhere Traffic
          Manager can take it (see `release`). A lapsed lease or a closed tab
          is a truck nobody is steering, not an emergency.
        - A claim carrying only the resting state leaves Traffic Manager in
          control rather than braking the truck during takeover.
        - A driver that supplied input drives the accepted command state.
        """
        if accepted.estopped:
            self._disengage_autonomy()
            self._vehicle.apply_control(carla.VehicleControl(throttle=0.0, brake=1.0, steer=0.0, hand_brake=True))
            return

        if not has_driver:
            self.release()
            return

        # A player without feedback yet claims with the resting state. Applying
        # that frame would make every click brake an autonomous truck.
        if self._autonomous and accepted.state == DriveState.stopped():
            return

        self._disengage_autonomy()
        state = accepted.state
        # CARLA has no neutral, so it is expressed as zero throttle: a truck in
        # neutral coasts and still brakes and steers, rather than driving.
        throttle = 0.0 if state.gear is Gear.NEUTRAL else state.throttle
        self._vehicle.apply_control(
            carla.VehicleControl(
                throttle=throttle,
                brake=state.brake,
                steer=state.steering,
                reverse=state.gear is Gear.REVERSE,
            )
        )

    def destroy(self) -> None:
        """Tear down every sensor before the vehicle they are attached to.

        Cleanup keeps walking after a failed actor RPC so one sensor the server
        already culled cannot leave the other sensor or vehicle behind.
        """
        failure: RuntimeError | None = None
        for sensor in (*self._cameras, self._collision_sensor):
            for cleanup in (sensor.stop, sensor.destroy):
                try:
                    cleanup()
                except RuntimeError as err:
                    failure = failure or err
        try:
            self._vehicle.destroy()
        except RuntimeError as err:
            failure = failure or err
        if failure is not None:
            raise failure


def next_deadline(deadline: float, now: float, period: float) -> float:
    """When the tick after the one just issued is due.

    Deadlines advance by the period from each other, not from `now`, so the
    cadence does not drift by the tick's own duration. A tick that overran by
    more than a whole period (a server hitch) re-anchors on `now` instead:
    catching up would fire the missed ticks back to back, and a burst of frames
    stamped a step apart is the very judder a fixed step exists to avoid. Sim
    time simply runs that much behind the wall clock, which nothing depends on.
    """
    deadline += period
    if deadline < now - period:
        return now
    return deadline


# Why the clock is being stepped. `Clock` holds are keyed by reason so the
# startup gate, a camera, a map viewer and a driver can each keep the world
# running without one of them releasing on another's behalf.
STARTUP_HOLD = "startup"
RENDER_HOLD = "render"


def drive_hold(truck: str) -> str:
    return f"drive:{truck}"


def status_hold(truck: str) -> str:
    return f"status:{truck}"


class Clock:
    """Drives the simulation one fixed step per wall-clock period.

    CARLA's own free-running mode stamps each camera frame with however long the
    previous server tick took, and with `sensor_tick` quantizing on top of that
    a nominal 30 fps camera delivered frames 11 ms and 53 ms apart in pairs.
    The player paints by those stamps, so the judder is on screen whatever the
    encoder or the network do. Synchronous mode with a fixed step makes every
    frame exactly a period apart, and this thread supplies the ticks at that
    period so sim time keeps pace with the wall clock.

    A tick failure is fatal for the session, the same as a lost video track: in
    synchronous mode a world nobody ticks is frozen, and every truck would go on
    looking healthy while publishing nothing. A tick that never returns is the
    same frozen world with no failure to report, so each one that does return
    beats `heartbeat` for the watchdog.

    Ticking is held rather than unconditional. Stepping physics and Traffic
    Manager costs a full core even with rendering parked, so a world no viewer
    and no driver needs stops stepping until one arrives. `hold` and `release`
    name their reason so two holders cannot cancel each other out; see
    `World.ready` for the one that covers startup.
    """

    def __init__(self, world: carla.World, fps: int) -> None:
        self._world = world
        self._period = 1.0 / fps
        self.heartbeat = Heartbeat("simulation clock")
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="carla-clock", daemon=True)
        self._failure_loop: asyncio.AbstractEventLoop | None = None
        self._failure: asyncio.Future[BaseException] | None = None
        # Held through `sweep` and `spawn`, which wait on ticks. `World.ready`
        # drops it once every truck is in the world.
        self._holds: set[str] = {STARTUP_HOLD}
        self._holds_lock = threading.Lock()
        self._wanted = threading.Event()
        self._wanted.set()

    @property
    def running(self) -> bool:
        """Whether the world is currently being stepped."""
        return self._wanted.is_set()

    def hold(self, reason: str) -> None:
        """Step the world until `reason` is released. Idempotent."""
        with self._holds_lock:
            self._holds.add(reason)
            self._wanted.set()

    def release(self, reason: str) -> None:
        """Drop one hold, parking the world once the last one goes. Idempotent."""
        with self._holds_lock:
            self._holds.discard(reason)
            if not self._holds:
                self._wanted.clear()

    def start(self) -> None:
        self._failure_loop = asyncio.get_running_loop()
        self._failure = self._failure_loop.create_future()
        self._thread.start()

    async def wait_failed(self) -> None:
        """Raise when the clock can no longer tick the world."""
        if self._failure is None:
            raise RuntimeError("clock was not started")
        failure = await self._failure
        raise RuntimeError("simulation clock stopped") from failure

    def stop(self) -> None:
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=5.0)
        if self._failure is not None and not self._failure.done():
            self._failure.cancel()

    def _run(self) -> None:
        deadline = time.monotonic()
        # Tick durations since the last report. A tick longer than the period is
        # the server failing to render the active cameras in time, which shows
        # up as a slow-motion truck and nothing else would name the cause.
        ticks: list[float] = []
        reported = deadline
        parked = False
        while not self._stop.is_set():
            if not self._wanted.is_set():
                if not parked:
                    log.info("clock parked")
                    parked = True
                # The thread is idle by design, not wedged, so it keeps beating:
                # the watchdog exists for a `tick` that never returns, and would
                # otherwise kill the worker a minute into every parked world.
                # Waking each period rather than blocking also keeps `stop`
                # responsive during shutdown.
                self.heartbeat.beat()
                self._wanted.wait(self._period)
                # Whatever the pause cost, the next tick is due now rather than
                # owed a burst of catch-up steps; `next_deadline` collapses a
                # deadline this far behind to the current time.
                reported = time.monotonic()
                continue
            if parked:
                log.info("clock running")
                parked = False
            started = time.monotonic()
            try:
                self._world.tick()
            except RuntimeError as err:
                if self._failure_loop is not None:
                    self._failure_loop.call_soon_threadsafe(self._record_failure, err)
                log.error("simulation tick failed, stopping: %s", err)
                return
            now = time.monotonic()
            self.heartbeat.beat()
            ticks.append(now - started)
            if now - reported >= CLOCK_REPORT_INTERVAL:
                late = sum(1 for tick in ticks if tick > self._period)
                log.info(
                    "clock: %d ticks, mean %.1f ms, max %.1f ms, %d over the %.1f ms period",
                    len(ticks),
                    1000 * sum(ticks) / len(ticks),
                    1000 * max(ticks),
                    late,
                    1000 * self._period,
                )
                ticks.clear()
                reported = now
            deadline = next_deadline(deadline, now, self._period)
            self._stop.wait(max(0.0, deadline - now))

    def _record_failure(self, failure: BaseException) -> None:
        if self._failure is not None and not self._failure.done():
            self._failure.set_result(failure)


class World:
    """The shared CARLA world and the trucks spawned into it.

    The world runs in synchronous mode at a fixed step of `1 / fps`, ticked by
    `clock`; see `Clock` for why. The mode is restored on `destroy`, because a
    synchronous world with no client ticking it is frozen for the next one.
    """

    def __init__(self, client: carla.Client, traffic_manager_port: int, fps: int) -> None:
        self._client = client
        self._world = client.get_world()
        self._tm = client.get_trafficmanager(traffic_manager_port)
        self._tm_port = traffic_manager_port
        self._map: carla.Map | None = None
        self._rendering_trucks: set[str] = set()
        self.trucks: list[Truck] = []
        # The server outlives this worker and may have been left rendering, or
        # free-running, by a hard crash. Assert the idle state and the fixed
        # step before doing any other world work. Traffic Manager setup can
        # still fail after apply_settings has already frozen the world, so
        # roll that back rather than leave the next client a world nobody ticks.
        settings = self._world.get_settings()
        settings.no_rendering_mode = True
        settings.synchronous_mode = True
        settings.fixed_delta_seconds = 1.0 / fps
        self._world.apply_settings(settings)
        try:
            # Traffic Manager steps with the world, so a stalled tick stalls the
            # autonomous trucks rather than leaving them to drive through a frozen
            # frame.
            self._tm.set_synchronous_mode(True)
        except BaseException:
            try:
                settings = self._world.get_settings()
                settings.synchronous_mode = False
                settings.fixed_delta_seconds = 0.0
                self._world.apply_settings(settings)
            except RuntimeError as err:
                log.warning("failed to release synchronous mode after setup failed: %s", err)
            raise
        self.clock = Clock(self._world, fps)

    @classmethod
    def connect(cls, host: str, port: int, timeout: float, traffic_manager_port: int, fps: int) -> World:
        return cls(connect_client(host, port, timeout), traffic_manager_port, fps)

    def road_map(self) -> carla.Map:
        """The town, fetched once.

        `get_map` downloads and parses the whole OpenDRIVE, and both spawning and
        the map name want it, so hold on to it rather than paying twice at start.
        """
        if self._map is None:
            self._map = self._world.get_map()
        return self._map

    def map_name(self) -> str:
        """The loaded map's short name, e.g. `Town10HD_Opt`.

        CARLA reports it as a content path (`Carla/Maps/Town10HD_Opt`). The last
        segment is what `just carla map` names the road file it bakes,
        and what the player matches before drawing those roads under the trucks.
        """
        return self.road_map().name.rsplit("/", 1)[-1]

    def _set_render_demand(self, name: str, demanded: bool) -> None:
        """Render the shared world while at least one truck needs a camera."""
        next_trucks = set(self._rendering_trucks)
        if demanded:
            next_trucks.add(name)
        else:
            next_trucks.discard(name)
        if next_trucks == self._rendering_trucks:
            return

        was_rendering = bool(self._rendering_trucks)
        will_render = bool(next_trucks)
        if was_rendering != will_render:
            self._set_rendering(will_render)
            # Parking the renderer leaves physics and Traffic Manager stepping,
            # which is most of a core on its own, so the clock follows the same
            # demand. A driver or a map viewer holds it separately: see
            # `TruckSession`.
            if will_render:
                self.clock.hold(RENDER_HOLD)
            else:
                self.clock.release(RENDER_HOLD)
        self._rendering_trucks = next_trucks

    def ready(self) -> None:
        """Let the world park now that every truck is spawned.

        `sweep` and `spawn` wait on ticks, so the clock is held from
        construction until this drops the gate. Nothing has demanded a camera
        yet at that point, so the usual next step is straight to parked.
        """
        self.clock.release(STARTUP_HOLD)

    def _set_rendering(self, enabled: bool) -> None:
        settings = self._world.get_settings()
        no_rendering = not enabled
        if settings.no_rendering_mode == no_rendering:
            return
        settings.no_rendering_mode = no_rendering
        self._world.apply_settings(settings)
        log.info("CARLA rendering %s", "enabled" if enabled else "parked")

    def sweep(self) -> None:
        """Remove every vehicle a previous run left in the world, sensors first.

        The server outlives its clients, and Traffic Manager runs inside the
        client, so a worker that died without `destroy` leaves its trucks frozen
        wherever they were, holding whatever control they last had. They sit on
        the roads the next run spawns onto, and a fresh truck drives up behind
        one and waits there forever. This process is the world's only owner, so
        every vehicle in it is ours to remove.
        """
        actors = self._world.get_actors()
        vehicles = list(actors.filter("vehicle.*"))
        if not vehicles:
            return
        stale = {vehicle.id for vehicle in vehicles}
        sensors = [
            sensor for sensor in actors.filter("sensor.*") if sensor.parent is not None and sensor.parent.id in stale
        ]
        for sensor in sensors:
            for cleanup in (sensor.stop, sensor.destroy):
                try:
                    cleanup()
                except RuntimeError as err:
                    log.warning("failed to remove stale sensor %d: %s", sensor.id, err)
        # Every vehicle gets its attempt before the first failure is raised, so
        # one the server refuses does not leave the rest behind. A stale truck
        # still in the world is the jam this is here to clear, so spawning
        # beside it is not an option.
        stuck: list[str] = []
        for vehicle in vehicles:
            try:
                if not vehicle.destroy():
                    stuck.append(f"{vehicle.id}: server refused")
            except RuntimeError as err:
                stuck.append(f"{vehicle.id}: {err}")
        if stuck:
            raise RuntimeError("failed to remove stale vehicles: " + "; ".join(stuck))
        log.warning("removed %d vehicles and %d sensors left behind by a previous run", len(vehicles), len(sensors))
        # Destroys land on the next tick in synchronous mode. Spawn walks the
        # map immediately after, so wait for that tick or it still sees them.
        self._world.wait_for_tick(SPAWN_TICK_TIMEOUT)

    def spawn(self, names: list[str], camera: CameraConfig, camera_name: str) -> None:
        """Spawn one truck per name into the shared world, cameras attached.

        The first truck anchors a same-lane convoy. Every later truck first tries
        its position directly behind the lead, then falls back to the closest map
        spawn points if the road is obstructed. CARLA rejects physical overlap.

        Each truck's cameras render for its bridge stream, `truck/camera_name`.
        The simulator registers them with the bridge as they spawn, which is
        what starts it listening for this worker.
        """
        library = self._world.get_blueprint_library()
        if not library.find("sensor.camera.rgb").has_attribute(BRIDGE_ATTRIBUTE):
            raise RuntimeError(
                f"the simulator's cameras have no {BRIDGE_ATTRIBUTE} attribute: the simulator "
                "lacks the GPU video bridge. Build it with https://github.com/moq-dev/carla and pin CARLA_IMAGE."
            )
        truck_bp = library.find(TRUCK_BLUEPRINT)
        # Traffic Manager deletes stuck non-hero vehicles. Every truck must
        # survive a jam until an operator takes over or respawns it; deleting
        # one also invalidates its sensors and telemetry. Hero still supports
        # autopilot and anchors streamed map tiles around the vehicle.
        truck_bp.set_attribute("role_name", "hero")

        road_map = self.road_map()
        points = road_map.get_spawn_points()
        if len(points) < len(names):
            raise RuntimeError(f"map has {len(points)} spawn points, need {len(names)}")
        anchors = convoy_anchor_points(road_map, points, len(names) - 1)

        lead_transform: carla.Transform | None = None
        for index, name in enumerate(names):
            if lead_transform is None:
                candidates = anchors
            else:
                convoy_point = convoy_transform(road_map, lead_transform, index)
                candidates = [convoy_point, *nearby_spawn_points(points, convoy_point.location)]

            vehicle = self._spawn_vehicle(truck_bp, candidates)
            cameras: list[carla.Sensor] = []
            # Every sensor spawned so far, in spawn order, so the failure paths
            # below can walk them without caring which kind each one is.
            sensors: list[carla.Sensor] = []
            try:
                # A synchronous spawn lands on the next tick, and the transform the
                # convoy is laid out from is read back from that tick's snapshot.
                self._world.wait_for_tick(SPAWN_TICK_TIMEOUT)
                if index == 0:
                    lead_transform = vehicle.get_transform()
                for rendition in camera.renditions():
                    camera_bp = library.find("sensor.camera.rgb")
                    camera_bp.set_attribute("image_size_x", str(rendition.width))
                    camera_bp.set_attribute("image_size_y", str(rendition.height))
                    # Every tick: the fixed step already is the frame period, and a
                    # tick-length `sensor_tick` on top would skip frames on rounding.
                    camera_bp.set_attribute("sensor_tick", "0.0")
                    camera_bp.set_attribute(BRIDGE_ATTRIBUTE, bridge_stream(name, camera_name))
                    # The simulator refuses the spawn when the camera cannot
                    # reach the bridge, naming why in its log.
                    cameras.append(self._world.spawn_actor(camera_bp, camera_transform(vehicle), attach_to=vehicle))
                    sensors.append(cameras[-1])
                # Attached at the vehicle origin: a collision sensor reports the
                # actor's contacts, so where on it the sensor sits is irrelevant.
                collision = self._world.spawn_actor(
                    library.find("sensor.other.collision"), carla.Transform(), attach_to=vehicle
                )
                sensors.append(collision)
            except BaseException:
                # The vehicle is in the world already and `destroy` only walks
                # the trucks it finished building, so failed sensors would
                # otherwise leave it parked there for the next run. Every exit
                # counts, a SIGTERM mid-RPC included, not just a refused spawn.
                for sensor in sensors:
                    try:
                        sensor.destroy()
                    except RuntimeError as err:
                        log.warning("failed to clean up sensor after %s sensor spawn failed: %s", name, err)
                try:
                    vehicle.destroy()
                except RuntimeError as err:
                    log.warning("failed to clean up vehicle after %s spawn failed: %s", name, err)
                raise
            try:
                truck = Truck(
                    name,
                    vehicle,
                    cameras,
                    collision,
                    self._tm_port,
                    self._set_render_demand,
                    road_map=road_map,
                )
            except BaseException:
                # Construction enables autopilot before the Truck reaches
                # `self.trucks`, so clean up every actor here rather than leave
                # them invisible to `destroy()` on the persistent server.
                cleanups = [
                    *(cleanup for sensor in sensors for cleanup in (sensor.stop, sensor.destroy)),
                    vehicle.destroy,
                ]
                for cleanup in cleanups:
                    try:
                        cleanup()
                    except RuntimeError as err:
                        log.warning("failed to clean up after %s construction failed: %s", name, err)
                raise
            self.trucks.append(truck)
            log.info("spawned %s (actor %d)", name, vehicle.id)

    def _spawn_vehicle(self, blueprint: carla.ActorBlueprint, points: list[carla.Transform]):
        """Spawn at the first free point in the caller's preferred order.

        `try_spawn_actor` returns None when the point is occupied. `sweep` has
        cleared the last run's trucks by now, so that is a convoy follower whose
        spot is blocked; walking the ordered candidates finds it the nearest
        free road, and still fails loudly if the map is genuinely full.
        """
        for point in points:
            vehicle = self._world.try_spawn_actor(blueprint, point)
            if vehicle is not None:
                return vehicle
        raise RuntimeError(f"every one of the {len(points)} candidate spawn points is occupied")

    def destroy(self) -> None:
        """Remove every actor this process spawned and hand the world back.

        The server outlives the client, so anything left behind stays in the
        world and the next run spawns beside it. The world goes back to
        free-running first: with the clock stopped nothing else would apply the
        destroys, and a synchronous world with no client is frozen for whatever
        connects next (`just carla map`, a hand-run client).
        """
        self.clock.stop()
        try:
            self._tm.set_synchronous_mode(False)
            settings = self._world.get_settings()
            settings.synchronous_mode = False
            settings.fixed_delta_seconds = 0.0
            self._world.apply_settings(settings)
        except RuntimeError as err:
            # A server already gone cannot be handed anything; the actors below
            # are gone with it, and their own errors say so.
            log.warning("failed to release synchronous mode: %s", err)
        for truck in self.trucks:
            try:
                truck.destroy()
            except RuntimeError as err:
                # A truck already gone (server restarted, actor culled) must not
                # stop the rest from being cleaned up.
                log.warning("failed to destroy %s: %s", truck.name, err)
        self.trucks.clear()
