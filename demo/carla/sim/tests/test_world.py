from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from moq_carla_sim import world as world_module
from moq_carla_sim.drive import Accepted, DriveState, Gear
from moq_carla_sim.publish import Collision, Motion, Pose
from moq_carla_sim.world import (
    CAMERA_STANDOFF,
    COLLISION_WINDOW,
    CONVOY_SPACING,
    SPAWN_TICK_TIMEOUT,
    CameraConfig,
    Clock,
    Truck,
    World,
    camera_transform,
    convoy_anchor_points,
    convoy_transform,
    free_spawn_point,
    nearby_spawn_points,
    next_deadline,
)


def vector(x: float = 0.0, y: float = 0.0, z: float = 0.0) -> SimpleNamespace:
    return SimpleNamespace(x=x, y=y, z=z)


def fake_sensor(events: list[str] | None = None, name: str = "sensor") -> SimpleNamespace:
    """A sensor whose lifecycle calls are recorded, or ignored."""
    record = (lambda call: events.append(f"{name}.{call}")) if events is not None else (lambda _call: None)
    return SimpleNamespace(
        listen=lambda _callback: record("listen"),
        stop=lambda: record("stop"),
        destroy=lambda: record("destroy"),
    )


def fake_traffic_manager(modes: list[bool] | None = None) -> SimpleNamespace:
    return SimpleNamespace(set_synchronous_mode=lambda sync: modes is not None and modes.append(sync))


def fake_vehicle(**overrides) -> SimpleNamespace:
    """A stationary vehicle pointing along +x, applying nothing."""
    defaults = dict(
        id=1,
        set_autopilot=lambda *_args: None,
        get_transform=lambda: SimpleNamespace(
            location=vector(),
            rotation=SimpleNamespace(yaw=0.0),
            get_forward_vector=lambda: vector(x=1.0),
            get_right_vector=lambda: vector(y=1.0),
        ),
        get_velocity=lambda: vector(),
        get_acceleration=lambda: vector(),
        get_angular_velocity=lambda: vector(),
        get_control=lambda: SimpleNamespace(steer=0.0, throttle=0.0, brake=0.0, reverse=False),
    )
    vehicle = SimpleNamespace(**{**defaults, **overrides})
    if "get_world" not in overrides:
        vehicle.get_world = lambda: SimpleNamespace(
            get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [vehicle.get_transform()]),
            get_actors=lambda: SimpleNamespace(filter=lambda _pattern: [vehicle]),
        )
    return vehicle


def test_camera_config_builds_240p_sd_and_720p_hd_by_default() -> None:
    assert CameraConfig().renditions() == (
        world_module.RenditionConfig(name="sd", width=426, height=240, fps=30, bitrate=800_000),
        world_module.RenditionConfig(name="hd", width=1280, height=720, fps=30, bitrate=4_000_000),
    )


def test_pose_reports_world_metres_and_speed() -> None:
    vehicle = fake_vehicle(
        get_transform=lambda: SimpleNamespace(
            location=vector(x=12.5, y=-3.25),
            rotation=SimpleNamespace(yaw=90.0),
            get_forward_vector=lambda: vector(y=1.0),
            get_right_vector=lambda: vector(x=-1.0),
        ),
        get_velocity=lambda: vector(x=3.0, y=4.0),
    )

    telemetry = Truck("truck1", vehicle, [], fake_sensor(), 8000, road_map=SimpleNamespace()).telemetry()

    assert telemetry.pose == Pose(x=12.5, y=-3.25, yaw=90.0, speed=5.0)


def test_measured_control_is_what_the_physics_applied_not_the_command() -> None:
    """`get_control` is populated whoever is driving, Traffic Manager included."""
    vehicle = fake_vehicle(get_control=lambda: SimpleNamespace(steer=-0.4, throttle=0.6, brake=0.0, reverse=True))

    measured = Truck("truck1", vehicle, [], fake_sensor(), 8000, road_map=SimpleNamespace()).telemetry().measured

    assert (measured.steering, measured.throttle, measured.brake) == (-0.4, 0.6, 0.0)
    assert measured.gear is Gear.REVERSE


def test_respawn_returns_to_spawn_and_reengages_autonomy() -> None:
    events: list[object] = []
    spawn = SimpleNamespace(
        name="spawn",
        location=vector(),
        rotation=SimpleNamespace(yaw=0.0),
        get_forward_vector=lambda: vector(x=1.0),
        get_right_vector=lambda: vector(y=1.0),
    )
    vehicle = fake_vehicle(
        get_transform=lambda: spawn,
        set_autopilot=lambda enabled, port: events.append(("autopilot", enabled, port)),
        apply_control=lambda _control: events.append("control.reset"),
        set_transform=lambda transform: events.append(("transform", transform)),
        set_target_velocity=lambda _velocity: events.append("velocity.reset"),
        set_target_angular_velocity=lambda _velocity: events.append("angular.reset"),
        get_world=lambda: SimpleNamespace(
            get_map=lambda: pytest.fail("respawn downloads the map again"),
            get_actors=lambda: SimpleNamespace(filter=lambda _pattern: [vehicle]),
        ),
    )
    road_map = SimpleNamespace(get_spawn_points=lambda: [spawn])
    truck = Truck("truck1", vehicle, [], fake_sensor(), 8000, road_map=road_map)
    truck._collision = Collision(at=1.0, other="barrier", impulse=2.0)
    events.clear()

    truck.respawn()

    assert events == [
        ("autopilot", False, 8000),
        "control.reset",
        ("transform", spawn),
        "velocity.reset",
        "angular.reset",
        ("autopilot", True, 8000),
    ]
    assert truck.autonomous
    assert truck.telemetry().collision is None


def test_acceleration_is_projected_onto_the_vehicles_own_axes() -> None:
    """A truck facing south feels a westward push as acceleration to its right."""
    vehicle = fake_vehicle(
        get_transform=lambda: SimpleNamespace(
            location=vector(),
            rotation=SimpleNamespace(yaw=90.0),
            get_forward_vector=lambda: vector(y=1.0),
            get_right_vector=lambda: vector(x=-1.0),
        ),
        get_acceleration=lambda: vector(x=-3.0, y=2.0),
        get_angular_velocity=lambda: vector(z=7.5),
    )

    assert Truck("truck1", vehicle, [], fake_sensor(), 8000, road_map=SimpleNamespace()).telemetry().motion == Motion(
        yaw_rate=7.5, accel_lon=2.0, accel_lat=3.0
    )


def test_acceleration_on_a_grade_keeps_its_vertical_component() -> None:
    """A road that is graded and banked is where the z terms live."""
    # Nose up a 3-4-5 grade: forward tilts out of the horizontal plane, and the
    # truck accelerating along that slope has acceleration pointing up it too.
    vehicle = fake_vehicle(
        get_transform=lambda: SimpleNamespace(
            location=vector(),
            rotation=SimpleNamespace(yaw=0.0),
            get_forward_vector=lambda: vector(x=0.8, z=0.6),
            get_right_vector=lambda: vector(y=1.0),
        ),
        get_acceleration=lambda: vector(x=4.0, z=3.0),
    )

    motion = Truck("truck1", vehicle, [], fake_sensor(), 8000, road_map=SimpleNamespace()).telemetry().motion

    # 4.0*0.8 + 3.0*0.6; dropping the vertical term would report 3.2.
    assert motion.accel_lon == pytest.approx(5.0)


class Contacts:
    """Drive `_on_collision` with both clocks under the test's control."""

    def __init__(self, truck: Truck, patch: pytest.MonkeyPatch) -> None:
        self.truck = truck
        self.monotonic = 100.0
        self.wall = 1756334400000.0
        patch.setattr(world_module.time, "monotonic", lambda: self.monotonic)
        patch.setattr(world_module.time, "time", lambda: self.wall / 1000.0)

    def advance(self, seconds: float) -> None:
        self.monotonic += seconds
        self.wall += seconds * 1000.0

    def hit(self, magnitude: float, other: str, actor: int = 1) -> None:
        self.truck._on_collision(
            SimpleNamespace(
                normal_impulse=vector(x=magnitude),
                other_actor=SimpleNamespace(id=actor, type_id=other),
            )
        )


def test_continuous_contact_is_one_collision_with_its_peak_impulse() -> None:
    """CARLA emits an event per frame of contact; a barrier is not 30 crashes."""
    truck = Truck("truck1", fake_vehicle(), [], fake_sensor(), 8000, road_map=SimpleNamespace())

    with pytest.MonkeyPatch.context() as patch:
        contacts = Contacts(truck, patch)
        started = contacts.wall
        contacts.hit(400.0, "static.sidewalk")
        contacts.advance(COLLISION_WINDOW / 2)
        contacts.hit(900.0, "static.sidewalk")
        contacts.advance(COLLISION_WINDOW / 2)
        contacts.hit(100.0, "static.sidewalk")
        # The timestamp stays at the start of contact: moving it forward as the
        # scrape continues would republish one collision as a stream of them.
        assert truck.telemetry().collision == Collision(at=started, other="static.sidewalk", impulse=900.0)

        # Contact keeps extending the window, so only a real gap starts a second.
        contacts.advance(COLLISION_WINDOW * 2)
        assert truck.telemetry().collision.at == started
        contacts.hit(50.0, "vehicle.tesla.model3", actor=2)
        assert truck.telemetry().collision == Collision(at=contacts.wall, other="vehicle.tesla.model3", impulse=50.0)


def test_striking_something_else_is_a_new_collision_however_soon() -> None:
    """Scrape a barrier, then hit a truck: two events, not one."""
    truck = Truck("truck1", fake_vehicle(), [], fake_sensor(), 8000, road_map=SimpleNamespace())

    with pytest.MonkeyPatch.context() as patch:
        contacts = Contacts(truck, patch)
        contacts.hit(400.0, "static.guardrail", actor=7)
        contacts.advance(COLLISION_WINDOW / 4)
        contacts.hit(50.0, "vehicle.carlamotors.european_hgv", actor=9)

        # The second is quieter and well inside the window, so a time-only
        # window would have hidden it behind the guardrail's own event.
        assert truck.telemetry().collision == Collision(
            at=contacts.wall, other="vehicle.carlamotors.european_hgv", impulse=50.0
        )


def test_two_of_the_same_model_are_two_collisions() -> None:
    """The window keys on the actor's id; `type_id` is a blueprint name."""
    truck = Truck("truck1", fake_vehicle(), [], fake_sensor(), 8000, road_map=SimpleNamespace())

    with pytest.MonkeyPatch.context() as patch:
        contacts = Contacts(truck, patch)
        contacts.hit(100.0, "vehicle.tesla.model3", actor=3)
        first = truck.telemetry().collision
        contacts.advance(COLLISION_WINDOW / 4)
        contacts.hit(100.0, "vehicle.tesla.model3", actor=4)

        assert truck.telemetry().collision.at != first.at


def test_a_truck_that_has_hit_nothing_reports_no_collision() -> None:
    truck = Truck("truck1", fake_vehicle(), [], fake_sensor(), 8000, road_map=SimpleNamespace())
    assert truck.telemetry().collision is None


def test_camera_sits_beyond_vehicle_front() -> None:
    vehicle = SimpleNamespace(
        bounding_box=SimpleNamespace(
            location=SimpleNamespace(x=0.01),
            extent=SimpleNamespace(x=3.97),
        )
    )

    transform = camera_transform(vehicle)

    assert transform.location.x == 0.01 + 3.97 + CAMERA_STANDOFF
    assert transform.location.z == 2.8


def test_nearby_spawn_points_cluster_around_first_truck() -> None:
    def point(x: float, y: float) -> SimpleNamespace:
        return SimpleNamespace(location=SimpleNamespace(x=x, y=y, z=0.0))

    far = point(100.0, 100.0)
    closest = point(12.0, 10.0)
    anchor = point(10.0, 10.0)
    middle = point(20.0, 10.0)

    ordered = nearby_spawn_points([far, closest, anchor, middle], SimpleNamespace(x=10.0, y=10.0, z=0.0))

    assert ordered == [anchor, closest, middle, far]


def test_free_spawn_point_skips_an_original_occupied_by_another_truck() -> None:
    original = SimpleNamespace(location=vector())
    fallback = SimpleNamespace(location=vector(x=20.0))
    occupying = SimpleNamespace(id=2, get_transform=lambda: original)

    assert free_spawn_point([original, fallback], [occupying], own_id=1) is fallback


def test_convoy_transform_places_trucks_behind_lead_in_same_lane() -> None:
    lead = SimpleNamespace(location="lead")
    follower = SimpleNamespace(
        road_id=1,
        section_id=0,
        lane_id=-1,
        s=88.0,
        transform=SimpleNamespace(location="follower", rotation="same lane"),
    )
    lead_waypoint = SimpleNamespace(
        road_id=1, lane_id=-1, previous=lambda distance: [follower] if distance == 2 * CONVOY_SPACING else []
    )
    road_map = SimpleNamespace(get_waypoint=lambda location: lead_waypoint if location == "lead" else None)

    transform = convoy_transform(road_map, lead, 2)

    assert transform is follower.transform


def test_convoy_anchor_points_exclude_junction_crossings() -> None:
    safe = SimpleNamespace(location="safe")
    junction = SimpleNamespace(location="junction")
    safe_follower = SimpleNamespace(
        road_id=1,
        section_id=0,
        lane_id=-1,
        s=0.0,
        transform=SimpleNamespace(),
    )
    waypoints = {
        "safe": SimpleNamespace(road_id=1, lane_id=-1, previous=lambda _distance: [safe_follower]),
        "junction": SimpleNamespace(road_id=2, lane_id=-1, previous=lambda _distance: [safe_follower]),
    }
    road_map = SimpleNamespace(get_waypoint=lambda location: waypoints[location])

    assert convoy_anchor_points(road_map, [junction, safe], followers=1) == [safe]


def test_resting_claim_preserves_autonomy_until_driver_input() -> None:
    autopilot = []
    controls = []
    vehicle = fake_vehicle(
        set_autopilot=lambda enabled, port: autopilot.append((enabled, port)),
        apply_control=controls.append,
    )
    camera = fake_sensor()
    truck = Truck("truck1", vehicle, [camera, camera], fake_sensor(), 8000, road_map=SimpleNamespace())

    truck.actuate(Accepted(state=DriveState.stopped()), has_driver=True)

    assert truck.autonomous
    assert autopilot == [(True, 8000)]
    assert controls == []

    truck.actuate(
        Accepted(state=DriveState(throttle=0.2, brake=0.0, gear=Gear.FORWARD)),
        has_driver=True,
    )

    assert not truck.autonomous
    assert autopilot[-1] == (False, 8000)
    assert controls[-1].throttle == 0.2


def test_released_truck_merges_onto_its_lane_before_autonomy_resumes() -> None:
    autopilot = []
    controls = []
    # A lane along +x through y=0; the truck starts 2 m to its left at speed.
    forward = vector(x=1.0)

    def ahead(distance: float) -> SimpleNamespace:
        return SimpleNamespace(
            transform=SimpleNamespace(location=vector(x=distance), get_forward_vector=lambda: forward),
            is_junction=False,
            next=lambda step: [ahead(distance + step)],
        )

    lane = SimpleNamespace(
        transform=SimpleNamespace(location=vector(), get_forward_vector=lambda: forward),
        lane_type="driving",
        is_junction=False,
        get_left_lane=lambda: None,
        get_right_lane=lambda: None,
        next=lambda distance: [ahead(distance)],
    )
    location = vector(y=-2.0)
    vehicle = fake_vehicle(
        set_autopilot=lambda enabled, port: autopilot.append((enabled, port)),
        apply_control=controls.append,
        get_transform=lambda: SimpleNamespace(
            location=location,
            rotation=SimpleNamespace(yaw=0.0),
            get_forward_vector=lambda: forward,
            get_right_vector=lambda: vector(y=1.0),
        ),
        get_velocity=lambda: vector(x=10.0),
        get_control=lambda: SimpleNamespace(steer=0.4, throttle=0.6, brake=0.0, reverse=False),
    )
    camera = fake_sensor()
    road_map = SimpleNamespace(get_waypoint=lambda _location, **_kwargs: lane)
    truck = Truck("truck1", vehicle, [camera, camera], fake_sensor(), 8000, road_map=road_map)
    truck.actuate(Accepted(state=DriveState(throttle=0.6, steering=0.4, gear=Gear.FORWARD)), has_driver=True)
    assert not truck.autonomous

    # Letting go off the lane coasts toward it rather than handing Traffic
    # Manager a truck it would swerve.
    truck.actuate(Accepted(state=DriveState.stopped()), has_driver=False)

    assert not truck.autonomous
    assert autopilot == [(True, 8000), (False, 8000)]
    assert controls[-1].steer > 0.0
    assert controls[-1].throttle == 0.0 and controls[-1].brake == 0.0

    # On the lane: autonomy resumes, once.
    location.y = 0.2
    truck.release()
    assert truck.autonomous
    assert autopilot[-1] == (True, 8000)

    truck.release()
    assert autopilot.count((True, 8000)) == 2


def test_truck_video_demand_renders_without_listening_to_a_camera() -> None:
    events: list[str] = []
    demand: list[tuple[str, bool]] = []
    cameras = [fake_sensor(events, name) for name in ("sd", "hd")]
    # The collision sensor listens for the truck's life. The cameras never do:
    # their pictures go to the bridge, not to this process.
    truck = Truck(
        "truck1",
        fake_vehicle(),
        cameras,
        fake_sensor(events, "collision"),
        8000,
        lambda name, demanded: demand.append((name, demanded)),
        road_map=SimpleNamespace(),
    )

    truck.set_video_demand(True)
    truck.set_video_demand(True)
    truck.set_video_demand(False)

    assert events == ["collision.listen"]
    assert demand == [("truck1", True), ("truck1", False)]


def test_world_renders_only_while_any_truck_camera_is_demanded() -> None:
    applied: list[bool] = []
    settings = SimpleNamespace(no_rendering_mode=False)
    carla_world = SimpleNamespace(
        get_settings=lambda: settings,
        apply_settings=lambda value: applied.append(value.no_rendering_mode),
    )
    client = SimpleNamespace(
        get_world=lambda: carla_world,
        get_trafficmanager=lambda _port: fake_traffic_manager(),
    )
    world = World(client, 8000, 30)
    trucks = [
        Truck(
            name,
            fake_vehicle(),
            [fake_sensor(), fake_sensor()],
            fake_sensor(),
            8000,
            world._set_render_demand,
            road_map=SimpleNamespace(),
        )
        for name in ("truck1", "truck2")
    ]

    trucks[0].set_video_demand(True)
    trucks[1].set_video_demand(True)
    trucks[0].set_video_demand(False)
    trucks[1].set_video_demand(False)

    # Initial idle, first viewer, and last viewer only. Demand changing while
    # another truck is visible does not flap the world.
    assert applied == [True, False, True]


def test_failed_render_transition_does_not_commit_demand() -> None:
    no_rendering = True
    attempts = 0

    def get_settings() -> SimpleNamespace:
        return SimpleNamespace(no_rendering_mode=no_rendering)

    def apply_settings(settings: SimpleNamespace) -> None:
        nonlocal attempts, no_rendering
        attempts += 1
        # The first apply is the constructor's fixed-step setup; fail the demand one.
        if attempts == 2:
            raise RuntimeError("CARLA rejected settings")
        no_rendering = settings.no_rendering_mode

    client = SimpleNamespace(
        get_world=lambda: SimpleNamespace(
            get_settings=get_settings,
            apply_settings=apply_settings,
        ),
        get_trafficmanager=lambda _port: fake_traffic_manager(),
    )
    world = World(client, 8000, 30)

    with pytest.raises(RuntimeError, match="CARLA rejected settings"):
        world._set_render_demand("truck1", True)

    assert world._rendering_trucks == set()
    assert no_rendering

    world._set_render_demand("truck1", True)

    assert world._rendering_trucks == {"truck1"}
    assert not no_rendering


def test_truck_constructor_failure_cleans_spawned_actors(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events = []
    blueprint = SimpleNamespace(set_attribute=lambda *_args: None, has_attribute=lambda _name: True)
    transform = SimpleNamespace(location=SimpleNamespace(x=0.0, y=0.0, z=0.0))
    vehicle = SimpleNamespace(
        id=1,
        bounding_box=SimpleNamespace(location=SimpleNamespace(x=0.0), extent=SimpleNamespace(x=4.0)),
        get_transform=lambda: transform,
        destroy=lambda: events.append("vehicle.destroy"),
    )
    sensors = iter(fake_sensor(events, name) for name in ("sd", "hd", "collision"))
    carla_world = SimpleNamespace(
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _name: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [transform]),
        try_spawn_actor=lambda _blueprint, _transform: vehicle,
        wait_for_tick=lambda _timeout: None,
        spawn_actor=lambda *_args, **_kwargs: next(sensors),
    )
    world = World.__new__(World)
    world._world = carla_world
    world._tm_port = 8000
    world._map = None
    world.trucks = []
    monkeypatch.setattr(
        world_module,
        "Truck",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(RuntimeError("autopilot failed")),
    )

    with pytest.raises(RuntimeError, match="autopilot failed"):
        world.spawn(["truck1"], world_module.CameraConfig(), "front")

    assert events == [
        "sd.stop",
        "sd.destroy",
        "hd.stop",
        "hd.destroy",
        "collision.stop",
        "collision.destroy",
        "vehicle.destroy",
    ]


@pytest.mark.parametrize("interrupt", [RuntimeError("collision spawn failed"), KeyboardInterrupt()])
def test_sensor_spawn_failure_attempts_every_cleanup(interrupt: BaseException) -> None:
    """A refused spawn and a SIGTERM mid-RPC both leave the vehicle in the world unless cleaned here."""
    events = []
    blueprint = SimpleNamespace(set_attribute=lambda *_args: None, has_attribute=lambda _name: True)
    transform = SimpleNamespace(location=SimpleNamespace(x=0.0, y=0.0, z=0.0))
    vehicle = SimpleNamespace(
        id=1,
        bounding_box=SimpleNamespace(location=SimpleNamespace(x=0.0), extent=SimpleNamespace(x=4.0)),
        get_transform=lambda: transform,
        destroy=lambda: events.append("vehicle.destroy"),
    )

    def fail_sensor_destroy() -> None:
        events.append("sd.destroy")
        raise RuntimeError("sensor already gone")

    # The collision sensor is spawned last, so its failure is the case where
    # cameras are already in the world and one of them refuses to be destroyed.
    spawned = iter(
        (
            SimpleNamespace(destroy=fail_sensor_destroy),
            SimpleNamespace(destroy=lambda: events.append("hd.destroy")),
            interrupt,
        )
    )

    def spawn_actor(*_args, **_kwargs):
        result = next(spawned)
        if isinstance(result, BaseException):
            raise result
        return result

    carla_world = SimpleNamespace(
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _name: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [transform]),
        try_spawn_actor=lambda _blueprint, _transform: vehicle,
        wait_for_tick=lambda _timeout: None,
        spawn_actor=spawn_actor,
    )
    world = World.__new__(World)
    world._world = carla_world
    world._tm_port = 8000
    world._map = None
    world.trucks = []

    with pytest.raises(type(interrupt)):
        world.spawn(["truck1"], world_module.CameraConfig(), "front")

    assert events == ["sd.destroy", "hd.destroy", "vehicle.destroy"]


def test_spawn_tick_failure_destroys_the_vehicle() -> None:
    """The vehicle is in the world before its landing tick, and `destroy` cannot see it yet."""
    events = []
    blueprint = SimpleNamespace(set_attribute=lambda *_args: None, has_attribute=lambda _name: True)
    transform = SimpleNamespace(location=SimpleNamespace(x=0.0, y=0.0, z=0.0))
    vehicle = SimpleNamespace(id=1, destroy=lambda: events.append("vehicle.destroy"))

    def wait_for_tick(_timeout: float) -> None:
        raise RuntimeError("time-out while waiting for the simulator")

    world = World.__new__(World)
    world._world = SimpleNamespace(
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _name: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [transform]),
        try_spawn_actor=lambda _blueprint, _transform: vehicle,
        wait_for_tick=wait_for_tick,
        spawn_actor=lambda *_args, **_kwargs: pytest.fail("no sensor spawns before the tick"),
    )
    world._tm_port = 8000
    world._map = None
    world.trucks = []

    with pytest.raises(RuntimeError, match="time-out"):
        world.spawn(["truck1"], world_module.CameraConfig(), "front")

    assert events == ["vehicle.destroy"]


def test_every_truck_is_protected_from_traffic_manager_idle_deletion(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # CARLA 0.9.16's ALSM excludes role_name=hero from stuck-vehicle deletion.
    # Capture the role at spawn, before the shared blueprint is reused.
    attributes: dict[str, str] = {}
    roles: list[str] = []
    blueprint = SimpleNamespace(set_attribute=attributes.__setitem__, has_attribute=lambda _name: True)
    transform = SimpleNamespace(location=vector())

    def spawn_vehicle(_blueprint, _transform):
        roles.append(attributes["role_name"])
        return fake_vehicle(
            id=len(roles),
            bounding_box=SimpleNamespace(location=vector(), extent=vector(x=4)),
        )

    world = World.__new__(World)
    world._world = SimpleNamespace(
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _name: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [transform] * 3),
        try_spawn_actor=spawn_vehicle,
        wait_for_tick=lambda _timeout: None,
        spawn_actor=lambda *_args, **_kwargs: fake_sensor(),
    )
    world._tm_port = 8000
    world._map = None
    world.trucks = []
    monkeypatch.setattr(world_module, "convoy_anchor_points", lambda _map, points, _count: points)
    monkeypatch.setattr(world_module, "convoy_transform", lambda *_args: transform)

    world.spawn(["truck1", "truck2", "truck3"], CameraConfig(), "front")

    assert roles == ["hero", "hero", "hero"]
    assert all(truck._autonomous for truck in world.trucks)


def spawning_world(blueprint: SimpleNamespace, spawn_actor) -> World:
    transform = SimpleNamespace(location=vector())
    world = World.__new__(World)
    world._world = SimpleNamespace(
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _name: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [transform]),
        try_spawn_actor=lambda _blueprint, _transform: fake_vehicle(
            bounding_box=SimpleNamespace(location=vector(), extent=vector(x=4)),
            destroy=lambda: None,
        ),
        wait_for_tick=lambda _timeout: None,
        spawn_actor=spawn_actor,
    )
    world._tm_port = 8000
    world._map = None
    world.trucks = []
    return world


def test_each_camera_renders_for_its_trucks_bridge_stream() -> None:
    attributes: dict[str, str] = {}
    streams: list[str] = []
    blueprint = SimpleNamespace(set_attribute=attributes.__setitem__, has_attribute=lambda _name: True)

    def spawn_actor(*_args, **_kwargs):
        streams.append(attributes.get(world_module.BRIDGE_ATTRIBUTE, ""))
        return fake_sensor()

    spawning_world(blueprint, spawn_actor).spawn(["truck1"], CameraConfig(), "front")

    # One camera per rendition, then the collision sensor.
    assert streams[:2] == ["truck1/front", "truck1/front"]


def test_a_simulator_without_the_bridge_is_refused_before_spawning() -> None:
    spawned: list[object] = []
    blueprint = SimpleNamespace(set_attribute=lambda *_args: None, has_attribute=lambda _name: False)
    world = spawning_world(blueprint, lambda *args, **_kwargs: spawned.append(args))

    with pytest.raises(RuntimeError, match="lacks the GPU video bridge"):
        world.spawn(["truck1"], CameraConfig(), "front")

    assert spawned == []
    assert world.trucks == []


def test_sweep_removes_stale_vehicles_and_their_sensors_only() -> None:
    """A previous run's trucks and cameras go; an unattached sensor is not ours."""
    events: list[str] = []
    ticks: list[float] = []

    def actor(name: str, parent=None) -> SimpleNamespace:
        return SimpleNamespace(
            id=hash(name) & 0xFFFF,
            parent=parent,
            stop=lambda: events.append(f"{name}.stop"),
            destroy=lambda: events.append(f"{name}.destroy") is None,
        )

    stale = [actor("truck1"), actor("truck2")]
    sensors = [actor("truck1.camera", stale[0]), actor("truck2.collision", stale[1]), actor("aerial")]

    def fail_stop() -> None:
        events.append("truck1.camera.stop")
        raise RuntimeError("already culled")

    sensors[0].stop = fail_stop
    actors = {"vehicle.*": stale, "sensor.*": sensors}
    world = World.__new__(World)
    world._world = SimpleNamespace(
        get_actors=lambda: SimpleNamespace(filter=lambda pattern: actors[pattern]),
        wait_for_tick=lambda timeout: ticks.append(timeout),
    )

    world.sweep()

    assert events == [
        "truck1.camera.stop",
        "truck1.camera.destroy",
        "truck2.collision.stop",
        "truck2.collision.destroy",
        "truck1.destroy",
        "truck2.destroy",
    ]
    # Destroys land on the next tick in synchronous mode, before spawn walks the map.
    assert ticks == [SPAWN_TICK_TIMEOUT]


def test_sweep_of_an_empty_world_touches_nothing() -> None:
    world = World.__new__(World)
    world._world = SimpleNamespace(get_actors=lambda: SimpleNamespace(filter=lambda _pattern: []))

    world.sweep()


def test_sweep_fails_startup_when_a_stale_vehicle_survives() -> None:
    """A truck the server refuses to remove is the jam itself; try them all, then fail."""
    destroyed: list[int] = []
    stale = [
        SimpleNamespace(id=1, destroy=lambda: False),
        SimpleNamespace(id=2, destroy=lambda: (_ for _ in ()).throw(RuntimeError("timed out"))),
        SimpleNamespace(id=3, destroy=lambda: destroyed.append(3) is None),
    ]
    actors = {"vehicle.*": stale, "sensor.*": []}
    world = World.__new__(World)
    world._world = SimpleNamespace(get_actors=lambda: SimpleNamespace(filter=lambda pattern: actors[pattern]))

    with pytest.raises(RuntimeError, match="1: server refused; 2: timed out"):
        world.sweep()

    assert destroyed == [3]


def test_world_opens_synchronous_at_the_frame_period_and_hands_it_back() -> None:
    applied: list[tuple[bool, float, bool]] = []
    settings = SimpleNamespace(no_rendering_mode=False, synchronous_mode=False, fixed_delta_seconds=0.0)
    tm_modes: list[bool] = []
    client = SimpleNamespace(
        get_world=lambda: SimpleNamespace(
            get_settings=lambda: settings,
            apply_settings=lambda value: applied.append(
                (value.synchronous_mode, value.fixed_delta_seconds, value.no_rendering_mode)
            ),
        ),
        get_trafficmanager=lambda _port: fake_traffic_manager(tm_modes),
    )

    world = World(client, 8000, 30)
    assert applied == [(True, 1 / 30, True)]
    assert tm_modes == [True]

    world.destroy()
    assert applied[-1] == (False, 0.0, True)
    assert tm_modes == [True, False]


def test_traffic_manager_setup_failure_restores_free_running() -> None:
    """A TM that refuses sync must not leave the persistent world frozen."""
    applied: list[tuple[bool, float, bool]] = []
    settings = SimpleNamespace(no_rendering_mode=False, synchronous_mode=False, fixed_delta_seconds=0.0)

    def apply_settings(value: SimpleNamespace) -> None:
        applied.append((value.synchronous_mode, value.fixed_delta_seconds, value.no_rendering_mode))
        settings.synchronous_mode = value.synchronous_mode
        settings.fixed_delta_seconds = value.fixed_delta_seconds
        settings.no_rendering_mode = value.no_rendering_mode

    def set_synchronous_mode(sync: bool) -> None:
        if sync:
            raise RuntimeError("Traffic Manager unavailable")

    client = SimpleNamespace(
        get_world=lambda: SimpleNamespace(get_settings=lambda: settings, apply_settings=apply_settings),
        get_trafficmanager=lambda _port: SimpleNamespace(set_synchronous_mode=set_synchronous_mode),
    )

    with pytest.raises(RuntimeError, match="Traffic Manager unavailable"):
        World(client, 8000, 30)

    assert applied == [(True, 1 / 30, True), (False, 0.0, True)]
    assert (settings.synchronous_mode, settings.fixed_delta_seconds) == (False, 0.0)


def test_next_deadline_keeps_cadence_from_the_previous_deadline() -> None:
    # A tick that finished early or late leaves the next deadline untouched.
    assert next_deadline(10.0, 10.01, 0.1) == pytest.approx(10.1)
    assert next_deadline(10.0, 10.09, 0.1) == pytest.approx(10.1)


def test_next_deadline_reanchors_after_a_hitch_instead_of_bursting() -> None:
    # A tick that overran by more than a period does not owe the missed ticks.
    assert next_deadline(10.0, 10.25, 0.1) == pytest.approx(10.25)


def test_clock_failure_reaches_the_session() -> None:
    def tick() -> None:
        raise RuntimeError("server gone")

    async def scenario() -> None:
        clock = Clock(SimpleNamespace(tick=tick), 30)
        clock.start()
        try:
            with pytest.raises(RuntimeError, match="simulation clock stopped") as err:
                await asyncio.wait_for(clock.wait_failed(), timeout=1.0)
            assert "server gone" in str(err.value.__cause__)
        finally:
            clock.stop()

    asyncio.run(scenario())


def test_clock_is_held_until_every_truck_is_spawned() -> None:
    """`sweep` and `spawn` wait on ticks, so the world must step through them."""
    clock = Clock(SimpleNamespace(tick=lambda: None), 30)
    assert clock.running

    clock.release(world_module.STARTUP_HOLD)

    assert not clock.running


def test_clock_follows_camera_demand() -> None:
    settings = SimpleNamespace(no_rendering_mode=False)
    carla_world = SimpleNamespace(
        get_settings=lambda: settings,
        apply_settings=lambda value: setattr(settings, "no_rendering_mode", value.no_rendering_mode),
    )
    client = SimpleNamespace(
        get_world=lambda: carla_world,
        get_trafficmanager=lambda _port: fake_traffic_manager(),
    )
    world = World(client, 8000, 30)
    world.ready()

    assert not world.clock.running

    world._set_render_demand("truck1", True)
    assert world.clock.running

    # A second viewer must not let the first one's departure park the world.
    world._set_render_demand("truck2", True)
    world._set_render_demand("truck1", False)
    assert world.clock.running

    world._set_render_demand("truck2", False)
    assert not world.clock.running


def test_holds_do_not_release_on_each_others_behalf() -> None:
    clock = Clock(SimpleNamespace(tick=lambda: None), 30)
    clock.release(world_module.STARTUP_HOLD)

    clock.hold(world_module.RENDER_HOLD)
    clock.hold(world_module.drive_hold("truck1"))
    clock.release(world_module.RENDER_HOLD)

    # Nobody is watching, but a driver still holds the lease: freezing here
    # would accept the driver's commands and move nothing.
    assert clock.running

    clock.release(world_module.drive_hold("truck1"))
    assert not clock.running


def test_parked_clock_keeps_beating_so_the_watchdog_lets_it_rest() -> None:
    """A parked clock is idle by design; the watchdog only exists for a wedged one."""
    ticks = 0

    def tick() -> None:
        nonlocal ticks
        ticks += 1

    async def scenario() -> None:
        clock = Clock(SimpleNamespace(tick=tick), 30)
        clock.release(world_module.STARTUP_HOLD)
        clock.start()
        try:
            for _ in range(100):
                if clock.heartbeat.age() > 0.0:
                    break
                await asyncio.sleep(0.01)
            # Parked, but still proving the thread is alive.
            assert clock.heartbeat.age() < 0.5
            assert ticks == 0

            clock.hold(world_module.RENDER_HOLD)
            for _ in range(100):
                if ticks:
                    break
                await asyncio.sleep(0.01)
            assert ticks > 0
        finally:
            clock.stop()

    asyncio.run(scenario())
