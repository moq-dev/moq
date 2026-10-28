"""Measure camera throughput on an otherwise idle CARLA server (no MoQ required)."""

from __future__ import annotations

import argparse
import json
import math
import statistics
import threading
import time
from contextlib import ExitStack


class Frames:
    """Keep frame identities, not images, so delivery cannot hide behind tick FPS."""

    def __init__(self) -> None:
        self.condition = threading.Condition()
        self.received: dict[int, float] = {}
        self.duplicates = 0

    def receive(self, image) -> None:
        with self.condition:
            if image.frame in self.received:
                self.duplicates += 1
            else:
                self.received[image.frame] = time.monotonic()
            self.condition.notify_all()

    def finish(self, expected: dict[int, float], deadline: float) -> dict:
        with self.condition:
            self.condition.wait_for(
                lambda: expected.keys() <= self.received.keys(), max(0, deadline - time.monotonic())
            )
            missing = sorted(expected.keys() - self.received.keys())
            # Same client monotonic clock on both sides: delay from requesting
            # this world tick to receiving its image, not glass-to-glass latency.
            delays = sorted(
                (self.received[frame] - started) * 1000 for frame, started in expected.items() if frame in self.received
            )
            return {
                "delivered": len(expected) - len(missing),
                "missing": missing,
                "duplicates": self.duplicates,
                "callback_delay_p95_ms": delays[math.ceil(len(delays) * 0.95) - 1] if delays else None,
                "callback_delay_max_ms": max(delays) if delays else None,
            }


def summary(durations: list[float]) -> dict:
    ordered = sorted(durations)
    return {
        "tick_fps": len(durations) / sum(durations),
        "mean_ms": statistics.mean(durations) * 1000,
        "p50_ms": statistics.median(durations) * 1000,
        "p95_ms": ordered[math.ceil(len(ordered) * 0.95) - 1] * 1000,
        "max_ms": max(durations) * 1000,
        "over_30fps_budget": sum(duration > 1 / 30 for duration in durations),
    }


def require_idle(world, timeout: float) -> None:
    if world.get_settings().synchronous_mode:
        raise RuntimeError("benchmark requires an idle asynchronous server; another client may own its ticks")
    # A new client can see an empty actor cache before its first world snapshot.
    # Receive that snapshot without taking ownership of the simulation clock.
    world.wait_for_tick(timeout)
    actors = world.get_actors()
    if any(actors.filter(pattern) for pattern in ("vehicle.*", "walker.*", "sensor.*")):
        raise RuntimeError("benchmark requires an empty server; use a separate CARLA instance")


def destroy(actor) -> None:
    if not actor.destroy():
        raise RuntimeError(f"failed to destroy benchmark actor {actor.id}")


def measure(
    client, cameras: int, width: int, height: int, ticks: int, warmup: int, timeout: float, listen: bool
) -> dict:
    import carla

    world = client.get_world()
    require_idle(world, timeout)
    original = world.get_settings()
    settings = world.get_settings()
    settings.synchronous_mode = True
    settings.fixed_delta_seconds = 1 / 30
    settings.no_rendering_mode = False
    # ExitStack attempts every cleanup even if one actor RPC fails. Restore the
    # clock last, after removing only actors this measurement created.
    with ExitStack() as cleanup:
        cleanup.callback(world.apply_settings, original)
        world.apply_settings(settings)
        library = world.get_blueprint_library()
        vehicle = world.spawn_actor(
            library.find("vehicle.carlamotors.european_hgv"), world.get_map().get_spawn_points()[0]
        )
        cleanup.callback(destroy, vehicle)
        vehicle.set_simulate_physics(False)
        collectors = []
        for index in range(cameras):
            blueprint = library.find("sensor.camera.rgb")
            blueprint.set_attribute("image_size_x", str(width))
            blueprint.set_attribute("image_size_y", str(height))
            blueprint.set_attribute("sensor_tick", "0.0")
            sensor = world.spawn_actor(
                blueprint, carla.Transform(carla.Location(x=-6, z=3 + index * 0.1)), attach_to=vehicle
            )
            cleanup.callback(destroy, sensor)
            if listen:
                frames = Frames()
                collectors.append(frames)
                sensor.listen(frames.receive)
                cleanup.callback(sensor.stop)
        for _ in range(warmup):
            world.tick()
        durations = []
        expected = {}
        for _ in range(ticks):
            started = time.monotonic()
            frame = world.tick()
            durations.append(time.monotonic() - started)
            expected[frame] = started
        if len(expected) != ticks or max(expected) - min(expected) != ticks - 1:
            raise RuntimeError("nonconsecutive world frames; another client may be ticking this server")
        # GPU cameras are pipelined. Waiting for each same-tick image deadlocks
        # that pipeline and measures the client barrier instead of capacity.
        # Drain extra world ticks without counting their images or durations.
        for _ in range(8):
            world.tick()
        deadline = time.monotonic() + timeout
        delivery = [collector.finish(expected, deadline) for collector in collectors]
        result = {
            "cameras": cameras,
            "width": width,
            "height": height,
            "listening": listen,
            "ticks": ticks,
            "warmup": warmup,
            **summary(durations),
            "delivery": delivery,
        }
        print(json.dumps(result), flush=True)
        if any(item["missing"] or item["duplicates"] for item in delivery):
            raise RuntimeError("camera delivery was incomplete or duplicated; this is not a passing throughput result")
        return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=2200)
    parser.add_argument("--ticks", type=int, default=300)
    parser.add_argument("--warmup", type=int, default=60)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--cameras", type=int, nargs="+", default=[0, 1, 2, 3, 4])
    parser.add_argument("--sizes", nargs="+", default=["320x180"])
    parser.add_argument("--no-listen", action="store_true", help="control: spawn cameras without activating capture")
    args = parser.parse_args(argv)
    if args.ticks < 1 or args.warmup < 1 or any(count < 0 for count in args.cameras):
        parser.error("ticks and warmup must be positive, camera counts nonnegative")
    if not math.isfinite(args.timeout) or args.timeout <= 0 or not 1 <= args.port <= 65535:
        parser.error("timeout must be finite and positive, port must be 1..65535")
    sizes = []
    for size in args.sizes:
        try:
            width, height = map(int, size.split("x"))
            if not (1 <= width <= 4096 and 1 <= height <= 4096):
                raise ValueError
        except ValueError:
            parser.error("sizes must be WIDTHxHEIGHT with dimensions 1..4096")
        sizes.append((width, height))

    import carla

    client = carla.Client(args.host, args.port)
    client.set_timeout(args.timeout)
    if client.get_client_version() != client.get_server_version():
        raise RuntimeError("CARLA client/server versions differ")
    print(json.dumps({"server": client.get_server_version(), "map": client.get_world().get_map().name}), flush=True)
    for width, height in sizes:
        for cameras in args.cameras:
            measure(client, cameras, width, height, args.ticks, args.warmup, args.timeout, not args.no_listen)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
