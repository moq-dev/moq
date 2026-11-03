from __future__ import annotations

from types import SimpleNamespace

import pytest

from moq_carla_sim.roadmap import feature_collection, lane_points, road_lines, simplify


def waypoint(x: float, y: float, lane: tuple[int, int, int], following: list | None = None) -> SimpleNamespace:
    ahead = following or []
    return SimpleNamespace(
        road_id=lane[0],
        section_id=lane[1],
        lane_id=lane[2],
        transform=SimpleNamespace(location=SimpleNamespace(x=x, y=y)),
        next=lambda _step: ahead,
    )


def test_simplify_collapses_a_straight_run_to_its_ends() -> None:
    straight = [(float(x), 0.0) for x in range(50)]

    assert simplify(straight, 0.5) == [(0.0, 0.0), (49.0, 0.0)]


def test_simplify_keeps_a_corner_outside_the_tolerance() -> None:
    corner = [(0.0, 0.0), (5.0, 2.0), (10.0, 0.0)]

    assert simplify(corner, 0.5) == corner
    # The same corner inside a wider corridor is a straight line.
    assert simplify(corner, 3.0) == [(0.0, 0.0), (10.0, 0.0)]


def test_lane_walk_stops_where_the_lane_does() -> None:
    lane = (1, 0, -1)
    third = waypoint(4.0, 0.0, lane)
    second = waypoint(2.0, 0.0, lane, [third])
    first = waypoint(0.0, 0.0, lane, [second])

    assert lane_points(first, 2.0) == [(0.0, 0.0), (2.0, 0.0), (4.0, 0.0)]


def test_lane_walk_does_not_follow_the_next_lane() -> None:
    other = waypoint(2.0, 0.0, (2, 0, -1))
    first = waypoint(0.0, 0.0, (1, 0, -1), [other])

    assert lane_points(first, 2.0) == [(0.0, 0.0)]


def test_lane_walk_bounds_a_ring_road() -> None:
    lane = (1, 0, -1)
    looping = waypoint(0.0, 0.0, lane)
    looping.next = lambda _step: [looping]

    assert len(lane_points(looping, 2.0, limit=10)) == 11


def test_road_lines_drops_a_lane_too_short_to_draw() -> None:
    lane = (1, 0, -1)
    stub = waypoint(0.0, 0.0, lane)
    end = waypoint(10.0, 0.0, lane)
    drawable = waypoint(0.0, 5.0, lane, [end])

    assert road_lines(SimpleNamespace(get_topology=lambda: [(stub, stub), (drawable, end)])) == [
        [(0.0, 5.0), (10.0, 0.0)]
    ]


def test_feature_collection_bounds_every_lane() -> None:
    collection = feature_collection("Town10HD_Opt", [[(0.0, 0.0), (10.04, 5.0)], [(-3.0, 2.0), (1.0, -8.0)]])

    assert collection["name"] == "Town10HD_Opt"
    assert collection["bbox"] == [-3.0, -8.0, 10.0, 5.0]
    assert collection["features"][0]["geometry"]["coordinates"][0] == [[0.0, 0.0], [10.0, 5.0]]


def test_a_map_with_no_drivable_lane_is_an_error() -> None:
    with pytest.raises(RuntimeError, match="no drivable lanes"):
        feature_collection("Empty", [])
