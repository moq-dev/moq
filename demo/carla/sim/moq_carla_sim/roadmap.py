"""Bake the running CARLA town's road network into the web player's map.

Run once per town, against a running simulator, and commit the result:

    just carla map

The output is a GeoJSON FeatureCollection of lane centrelines, but its
coordinates are CARLA world METERS (x east, y south), not WGS84 degrees. That is
deliberate: the shipped towns carry a null OpenDRIVE georeference, so
`transform_to_geolocation` would hand back real coordinates for a meaningless
place, and the trucks' `pose` telemetry is in these same meters. The player
projects both with one transform. Reprojecting onto a real basemap is a decision
about where the demo pretends to be, and belongs with whoever makes it.

Lane centrelines rather than road edges because that is what `get_topology`
gives directly, and at town scale a bundle of centrelines reads as a road.
"""

from __future__ import annotations

import argparse
import json
import logging
import math
import sys
from pathlib import Path
from typing import Any

log = logging.getLogger(__name__)

# Metres between sampled waypoints along a lane, and the corridor within which
# the simplifier may then drop them again. 2 m tracks a tight junction curve;
# 0.5 m of error is invisible on a town-sized map but collapses straight roads
# from hundreds of points to two.
STEP = 2.0
TOLERANCE = 0.5

# Waypoints to walk before abandoning a lane. `next` follows a ring road back
# into the same lane forever, so the walk needs a bound rather than a rule.
MAX_LANE_POINTS = 4000

Point = tuple[float, float]


def _segment_distance(point: Point, start: Point, end: Point) -> float:
    """Perpendicular distance from `point` to the segment `start`-`end`."""
    dx, dy = end[0] - start[0], end[1] - start[1]
    span = math.hypot(dx, dy)
    if span == 0.0:
        return math.hypot(point[0] - start[0], point[1] - start[1])
    return abs(dy * point[0] - dx * point[1] + end[0] * start[1] - end[1] * start[0]) / span


def simplify(points: list[Point], tolerance: float) -> list[Point]:
    """Ramer-Douglas-Peucker: drop points no further than `tolerance` off the line.

    Iterative rather than recursive so a long straight lane cannot bottom out
    the interpreter on its own geometry.
    """
    if len(points) < 3:
        return list(points)

    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        first, last = stack.pop()
        if last <= first + 1:
            continue
        index, worst = max(
            ((i, _segment_distance(points[i], points[first], points[last])) for i in range(first + 1, last)),
            key=lambda pair: pair[1],
        )
        if worst > tolerance:
            keep[index] = True
            stack.append((first, index))
            stack.append((index, last))
    return [point for point, keep_it in zip(points, keep) if keep_it]


def lane_points(start: Any, step: float, limit: int = MAX_LANE_POINTS) -> list[Point]:
    """Walk one lane from `start` until it ends or turns into a different lane."""
    lane = (start.road_id, start.section_id, start.lane_id)
    waypoint = start
    points = [(waypoint.transform.location.x, waypoint.transform.location.y)]
    for _ in range(limit):
        following = waypoint.next(step)
        if not following:
            break
        waypoint = following[0]
        if (waypoint.road_id, waypoint.section_id, waypoint.lane_id) != lane:
            break
        points.append((waypoint.transform.location.x, waypoint.transform.location.y))
    return points


def road_lines(road_map: Any, step: float = STEP, tolerance: float = TOLERANCE) -> list[list[Point]]:
    """Every drivable lane in the map, as a simplified polyline.

    `get_topology` yields one entry per lane, so walking from each start covers
    the network exactly once.
    """
    lines = []
    for start, _end in road_map.get_topology():
        points = lane_points(start, step)
        if len(points) < 2:
            continue
        lines.append(simplify(points, tolerance))
    return lines


def feature_collection(name: str, lines: list[list[Point]]) -> dict[str, Any]:
    """Wrap lane polylines as GeoJSON, in CARLA meters (see the module docstring)."""
    if not lines:
        raise RuntimeError(f"{name} produced no drivable lanes")
    xs = [x for line in lines for x, _ in line]
    ys = [y for line in lines for _, y in line]
    return {
        "type": "FeatureCollection",
        "name": name,
        # Decimetres: finer than the map is ever drawn, and it halves the file.
        "bbox": [round(min(xs), 1), round(min(ys), 1), round(max(xs), 1), round(max(ys), 1)],
        "features": [
            {
                "type": "Feature",
                "properties": {"kind": "lane"},
                "geometry": {
                    "type": "MultiLineString",
                    "coordinates": [[[round(x, 1), round(y, 1)] for x, y in line] for line in lines],
                },
            }
        ],
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="carla-roadmap", description=__doc__)
    parser.add_argument("--carla-host", default="127.0.0.1")
    parser.add_argument("--carla-port", type=int, default=2000)
    parser.add_argument("--carla-timeout", type=float, default=30.0)
    parser.add_argument("--step", type=float, default=STEP, help="metres between sampled waypoints")
    parser.add_argument("--tolerance", type=float, default=TOLERANCE, help="metres of simplification error")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parents[2] / "web/src/maps",
        help="directory to write <map>.json into",
    )
    parser.add_argument("--log-level", default="info")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    logging.basicConfig(level=args.log_level.upper(), format="%(levelname)-5s %(name)s: %(message)s")

    # Imported here rather than at module scope so the pure geometry above stays
    # testable without the CARLA wheel.
    from .world import connect_client

    client = connect_client(args.carla_host, args.carla_port, args.carla_timeout)
    road_map = client.get_world().get_map()
    name = road_map.name.rsplit("/", 1)[-1]

    lines = road_lines(road_map, args.step, args.tolerance)
    collection = feature_collection(name, lines)

    args.out.mkdir(parents=True, exist_ok=True)
    path = args.out / f"{name}.json"
    path.write_text(json.dumps(collection, separators=(",", ":")) + "\n")
    points = sum(len(line) for line in lines)
    log.info("wrote %s: %d lanes, %d points, %d KiB", path, len(lines), points, path.stat().st_size // 1024)
    return 0


if __name__ == "__main__":
    sys.exit(main())
