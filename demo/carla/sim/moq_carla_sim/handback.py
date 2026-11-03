"""Steering a released truck onto a lane Traffic Manager will follow.

Traffic Manager seeds a vehicle it takes back from the waypoint nearest its
position, heading be damned, and steers hard for it (`LocalizationStage::Update`
via `InMemoryMap::GetWaypoint`; the client API has no say in that seed). A truck
centred on a lane that runs its way is handed over without a twitch. One
straddling the centre line, angled, or inside a junction is aimed at the
oncoming or crossing lane and swerves at whatever speed it has. So a released
truck first drives itself onto such a lane, coasting, and only then lets Traffic
Manager have it.

Pure geometry in CARLA's left-handed frame (x forward, y right, z up): steer is
positive to the right, and the sign of a 2D cross product says which side of the
truck a point lies on. Nothing here touches the simulator, so it is testable
without one.
"""

from __future__ import annotations

import math

import carla

# Traffic Manager takes a truck this close to a lane's centre line, and this well
# aligned with it, without a visible correction: the nearest waypoint is then on
# that lane, and the ones ahead of the truck are the ones it steers for.
LATERAL_TOLERANCE = 0.5
HEADING_TOLERANCE = math.radians(8.0)

# Pure pursuit aims at the lane two seconds ahead, bounded so a crawling truck
# still looks past its bumper and a fast one does not cut corners. Two seconds
# rather than one keeps the merge gentle: a 30 degree heading error at 36 km/h
# asks for about a third of the steering lock, not two thirds. The lane is
# walked to the target in steps, because `Waypoint.next` forks where the lane
# enters a junction and any one fork is an arbitrary exit; a target closer than
# the minimum means the junction is too near to merge before it.
LOOKAHEAD_SECONDS = 2.0
LOOKAHEAD_MIN = 8.0
LOOKAHEAD_MAX = 30.0
LOOKAHEAD_STEP = 2.0

# The HGV, near enough: pure pursuit needs a wheelbase and the road-wheel angle
# that `steer=1.0` commands.
WHEELBASE = 4.5
MAX_STEER = math.radians(35.0)

# Below walking pace Traffic Manager's correction is the same slow pull-away
# every truck makes from its spawn point, so a truck this slow is handed over
# wherever it stands rather than driven around looking for a lane.
WALKING_PACE = 1.0

# How many driving lanes to consider either side of the nearest one.
LANE_SEARCH = 6


def _dot(a, b) -> float:
    return a.x * b.x + a.y * b.y


def _cross(a, b) -> float:
    return a.x * b.y - a.y * b.x


def _length(a) -> float:
    return math.hypot(a.x, a.y)


def aligned_lane(road_map: carla.Map, location: carla.Location, forward: carla.Vector3D) -> carla.Waypoint | None:
    """The driving lane under `location` that runs closest to `forward`.

    `get_waypoint` returns the nearest lane by position, which for a truck near
    the centre line is the oncoming one as often as its own. Walk the road's
    driving lanes both ways from it and keep the best aligned.
    """
    nearest = road_map.get_waypoint(location, project_to_road=True, lane_type=carla.LaneType.Driving)
    if nearest is None:
        return None
    lanes = [nearest]
    for sideways in ("get_left_lane", "get_right_lane"):
        lane = getattr(nearest, sideways)()
        while lane is not None and lane.lane_type == carla.LaneType.Driving and len(lanes) < LANE_SEARCH:
            lanes.append(lane)
            lane = getattr(lane, sideways)()
    return max(lanes, key=lambda lane: _dot(lane.transform.get_forward_vector(), forward))


def pursuit_steer(location: carla.Location, forward: carla.Vector3D, target: carla.Location) -> float:
    """Pure-pursuit steering toward `target`, in CARLA's [-1, 1]."""
    to_target = carla.Vector3D(x=target.x - location.x, y=target.y - location.y, z=0.0)
    distance = math.hypot(to_target.x, to_target.y)
    if distance == 0.0:
        return 0.0
    alpha = math.atan2(_cross(forward, to_target), _dot(forward, to_target))
    delta = math.atan2(2.0 * WHEELBASE * math.sin(alpha), distance)
    return max(-1.0, min(1.0, delta / MAX_STEER))


def plan(
    road_map: carla.Map,
    transform: carla.Transform,
    velocity: carla.Vector3D,
    measured_steer: float,
    *,
    reversing: bool,
) -> carla.VehicleControl | None:
    """The released truck's control for this tick, or None once it can be handed over."""
    speed = math.sqrt(velocity.x**2 + velocity.y**2 + velocity.z**2)
    if speed <= WALKING_PACE:
        return None
    location = transform.location
    forward = transform.get_forward_vector()
    if reversing or _dot(velocity, forward) < 0.0:
        # Nothing here merges backwards. Stop, then hand over from rest.
        return carla.VehicleControl(brake=1.0, steer=measured_steer, reverse=reversing)

    lane = aligned_lane(road_map, location, forward)
    if lane is None or lane.is_junction:
        # Lanes cross inside a junction, so the nearest one means as little to
        # this as it does to Traffic Manager. Coast through on the driver's
        # steering and merge on the far side; the same below, when the junction
        # is too close ahead to merge before it.
        return carla.VehicleControl(throttle=0.0, brake=0.0, steer=measured_steer)

    lane_forward = lane.transform.get_forward_vector()
    offset = carla.Vector3D(x=location.x - lane.transform.location.x, y=location.y - lane.transform.location.y, z=0.0)
    lateral = _cross(lane_forward, offset)
    # Forward vectors are unit length in 3D; on a grade their 2D shadows are not.
    cosine = _dot(forward, lane_forward) / (_length(forward) * _length(lane_forward))
    heading = math.acos(max(-1.0, min(1.0, cosine)))
    if abs(lateral) <= LATERAL_TOLERANCE and heading <= HEADING_TOLERANCE:
        return None

    lookahead = max(LOOKAHEAD_MIN, min(LOOKAHEAD_MAX, speed * LOOKAHEAD_SECONDS))
    target = lane
    walked = 0.0
    while walked < lookahead:
        ahead = target.next(LOOKAHEAD_STEP)
        if len(ahead) != 1 or ahead[0].is_junction:
            break
        target = ahead[0]
        walked += LOOKAHEAD_STEP
    if walked < LOOKAHEAD_MIN:
        return carla.VehicleControl(throttle=0.0, brake=0.0, steer=measured_steer)
    return carla.VehicleControl(
        throttle=0.0, brake=0.0, steer=pursuit_steer(location, forward, target.transform.location)
    )
