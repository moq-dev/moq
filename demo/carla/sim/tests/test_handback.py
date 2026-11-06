"""The lane-merge a released truck performs before Traffic Manager takes it.

Geometry in CARLA's frame: x forward, y to the right, steer positive right.
"""

from __future__ import annotations

import math
from types import SimpleNamespace

from moq_carla_sim import handback


def vector(x: float = 0.0, y: float = 0.0, z: float = 0.0) -> SimpleNamespace:
    return SimpleNamespace(x=x, y=y, z=z)


def lane(
    x: float,
    y: float,
    yaw: float = 0.0,
    *,
    left=None,
    right=None,
    lane_type: str = "driving",
    junction: bool = False,
) -> SimpleNamespace:
    """A waypoint on a straight lane through (x, y) heading `yaw` degrees."""
    forward = vector(math.cos(math.radians(yaw)), math.sin(math.radians(yaw)))
    return SimpleNamespace(
        transform=SimpleNamespace(location=vector(x, y), get_forward_vector=lambda: forward),
        lane_type=lane_type,
        is_junction=junction,
        get_left_lane=lambda: left,
        get_right_lane=lambda: right,
        next=lambda distance: [lane(x + forward.x * distance, y + forward.y * distance, yaw)],
    )


def road_map(nearest) -> SimpleNamespace:
    return SimpleNamespace(get_waypoint=lambda _location, **_kwargs: nearest)


def transform(x: float, y: float, yaw: float = 0.0) -> SimpleNamespace:
    forward = vector(math.cos(math.radians(yaw)), math.sin(math.radians(yaw)))
    return SimpleNamespace(location=vector(x, y), get_forward_vector=lambda: forward)


def test_aligned_lane_prefers_the_lane_running_the_trucks_way_over_the_nearest() -> None:
    # Two-way road: the nearest lane is oncoming, its neighbour across the
    # centre line runs with the truck.
    own = lane(0.0, 3.5, 0.0)
    oncoming = lane(0.0, 0.0, 180.0, right=own)

    picked = handback.aligned_lane(road_map(oncoming), vector(0.0, 0.5), vector(1.0, 0.0))

    assert picked is own


def test_aligned_lane_stops_walking_at_a_non_driving_lane() -> None:
    shoulder = lane(0.0, -3.5, 0.0, lane_type="shoulder")
    own = lane(0.0, 0.0, 90.0, left=shoulder)

    assert handback.aligned_lane(road_map(own), vector(), vector(1.0, 0.0)) is own


def test_centred_and_aligned_truck_is_handed_over() -> None:
    control = handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, 0.3, 4.0), vector(10.0), 0.2, reversing=False)
    assert control is None


def test_offset_truck_steers_toward_the_lane_side() -> None:
    # Lane runs +x through y=0; the truck sits 2 m to its left (-y), so the
    # lane is to its right: positive steer, and the mirror case negative.
    right = handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, -2.0), vector(10.0), 0.0, reversing=False)
    left = handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, 2.0), vector(10.0), 0.0, reversing=False)

    assert 0.0 < right.steer <= 1.0
    assert left.steer == -right.steer
    assert right.throttle == 0.0 and right.brake == 0.0


def test_angled_truck_on_the_centre_line_steers_back() -> None:
    # Centred but pointing 30 degrees left of the lane: steer right, gently.
    control = handback.plan(
        road_map(lane(0.0, 0.0)), transform(0.0, 0.0, -30.0), vector(8.7, -5.0), 0.0, reversing=False
    )

    assert 0.0 < control.steer < 0.5


def test_junction_is_coasted_through_on_the_drivers_steering() -> None:
    control = handback.plan(
        road_map(lane(0.0, 0.0, junction=True)), transform(0.0, 3.0, 45.0), vector(7.0, 7.0), -0.4, reversing=False
    )

    assert control.steer == -0.4
    assert control.throttle == 0.0 and control.brake == 0.0


def test_junction_too_close_ahead_is_coasted_toward_on_the_drivers_steering() -> None:
    # The lane forks 4 m ahead: any fork is an arbitrary exit, so merging
    # toward it could be the very swerve this avoids.
    forking = lane(0.0, 0.0)
    forking.next = lambda distance: [lane(distance, 0.0), lane(distance, 0.0, 45.0)]

    control = handback.plan(road_map(forking), transform(0.0, 2.0), vector(10.0), 0.3, reversing=False)

    assert control.steer == 0.3


def test_target_stops_short_of_a_junction_far_enough_ahead() -> None:
    # Straight lane for 12 m, then a junction: the merge aims at 12 m, not 20.
    straight = lane(0.0, 0.0)

    def stepped(distance: float):
        point = lane(distance, 0.0)
        point.is_junction = distance > 12.0
        point.next = lambda step: [stepped(distance + step)]
        return point

    straight.next = lambda distance: [stepped(distance)]
    near = handback.plan(road_map(straight), transform(0.0, 2.0), vector(10.0), 0.0, reversing=False)
    far = handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, 2.0), vector(10.0), 0.0, reversing=False)

    assert near.steer < 0.0 and far.steer < 0.0
    assert near.steer < far.steer


def test_walking_pace_is_handed_over_wherever_it_is() -> None:
    assert handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, 5.0, 90.0), vector(0.9), 0.0, reversing=False) is None


def test_reversing_truck_brakes_to_a_stop_first() -> None:
    control = handback.plan(road_map(lane(0.0, 0.0)), transform(0.0, 0.0), vector(-4.0), 0.1, reversing=True)

    assert control.brake == 1.0
    assert control.reverse is True


def test_pursuit_steer_is_zero_straight_ahead_signed_by_side_and_clamped() -> None:
    at = vector()
    forward = vector(1.0, 0.0)
    assert handback.pursuit_steer(at, forward, vector(10.0, 0.0)) == 0.0
    assert handback.pursuit_steer(at, forward, vector(10.0, 3.0)) > 0.0
    assert handback.pursuit_steer(at, forward, vector(10.0, -3.0)) < 0.0
    # A target almost beside the truck asks for more lock than it has.
    assert handback.pursuit_steer(at, forward, vector(1.0, 5.0)) == 1.0
    assert handback.pursuit_steer(at, forward, vector(1.0, -5.0)) == -1.0
