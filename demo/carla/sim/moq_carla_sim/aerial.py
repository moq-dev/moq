"""Capture a calibrated aerial image without changing the running CARLA world."""

from __future__ import annotations

import argparse
import json
import math
import queue
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Camera:
    x: float
    y: float
    z: float
    pitch: float = -90.0
    yaw: float = -90.0
    fov: float = 20.0


def calibration(
    bbox: list[float], padding: float = 30.0, ground_z: float = 0.0, fov: float = 20.0
) -> tuple[Camera, list[float]]:
    """Fit a square pinhole footprint; image right is +x and image down is +y.

    The footprint is exact on the chosen ground plane. Elevated roofs and
    uneven terrain retain perspective displacement, so this is not an orthophoto.
    """
    if len(bbox) != 4 or not all(math.isfinite(value) for value in bbox):
        raise ValueError("bbox must contain four finite world coordinates")
    left, top, right, bottom = bbox
    if right <= left or bottom <= top:
        raise ValueError("bbox must have positive width and height")
    if not math.isfinite(padding) or padding < 0:
        raise ValueError("padding must be finite and nonnegative")
    if not math.isfinite(ground_z) or not math.isfinite(fov) or not 1 <= fov <= 90:
        raise ValueError("ground height must be finite and field of view must be 1..90 degrees")
    side = max(right - left, bottom - top) + 2 * padding
    x, y = (left + right) / 2, (top + bottom) / 2
    half = side / 2
    camera = Camera(x=x, y=y, z=ground_z + half / math.tan(math.radians(fov / 2)), fov=fov)
    return camera, [x - half, y - half, x + half, y + half]


def capture(world: Any, camera: Camera, resolution: int, timeout: float) -> Any:
    """Own just one temporary camera; the existing simulator owns all ticks."""
    import carla

    if resolution < 256 or resolution > 4096:
        raise ValueError("resolution must be 256..4096 pixels")
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("capture timeout must be finite and positive")
    if world.get_settings().no_rendering_mode:
        raise RuntimeError("CARLA rendering is parked; open a truck camera in the demo and rerun")
    blueprint = world.get_blueprint_library().find("sensor.camera.rgb")
    for name, value in {
        "image_size_x": str(resolution),
        "image_size_y": str(resolution),
        "fov": str(camera.fov),
        "sensor_tick": "0.5",
        "lens_k": "0",
        "lens_kcube": "0",
        "motion_blur_intensity": "0",
    }.items():
        blueprint.set_attribute(name, value)
    transform = carla.Transform(
        carla.Location(x=camera.x, y=camera.y, z=camera.z),
        carla.Rotation(pitch=camera.pitch, yaw=camera.yaw, roll=0),
    )
    frames: queue.Queue[Any] = queue.Queue(maxsize=1)

    def receive(frame: Any) -> None:
        try:
            frames.put_nowait(frame)
        except queue.Full:
            # A capture only needs one pending frame, never a backlog of images.
            return

    sensor = world.spawn_actor(blueprint, transform)
    try:
        sensor.listen(receive)
        deadline = time.monotonic() + timeout

        def next_frame() -> Any:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no aerial image before capture deadline")
            try:
                return frames.get(timeout=remaining)
            except queue.Empty as error:
                raise TimeoutError("no aerial image before capture deadline") from error

        # Discard initial exposure frames without driving or changing world time.
        for _ in range(3):
            next_frame()
        return next_frame()
    finally:
        try:
            sensor.stop()
        finally:
            if not sensor.destroy():
                raise RuntimeError(f"failed to destroy temporary aerial camera {sensor.id}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--carla-host", default="127.0.0.1")
    parser.add_argument("--carla-port", type=int, default=2000)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--resolution", type=int, default=2048)
    parser.add_argument("--padding", type=float, default=30.0, help="metres outside the baked roads")
    parser.add_argument("--ground-z", type=float, default=0.0, help="calibration plane in world metres")
    parser.add_argument("--fov", type=float, default=20.0)
    parser.add_argument("--out", type=Path, default=Path(__file__).resolve().parents[2] / "web/src/maps")
    args = parser.parse_args(argv)
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be finite and positive")

    import carla

    client = carla.Client(args.carla_host, args.carla_port)
    client.set_timeout(args.timeout)
    world = client.get_world()
    name = world.get_map().name.rsplit("/", 1)[-1]
    roads = json.loads((args.out / f"{name}.json").read_text())
    if roads["name"] != name:
        raise ValueError("baked road map name does not match the running world")
    camera, bbox = calibration(roads["bbox"], args.padding, args.ground_z, args.fov)
    frame = capture(world, camera, args.resolution, args.timeout)
    image = args.out / f"{name}.aerial.png"
    frame.save_to_disk(str(image))
    if not image.is_file() or image.stat().st_size == 0:
        raise RuntimeError(f"CARLA did not save an image to {image}")
    manifest = {
        "name": name,
        "image": image.name,
        "bbox": bbox,
        "width": frame.width,
        "height": frame.height,
        "groundZ": args.ground_z,
        "camera": asdict(camera),
        "frame": frame.frame,
    }
    path = args.out / f"{name}.aerial.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Wrote {image} and {path}; verify road alignment before shipping")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
