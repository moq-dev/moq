"""CLI entrypoint: one process, one CARLA world, every truck in it.

Deliberately one process for every truck rather than one per truck. CARLA
advises a single command-sending client, one owner keeps Traffic Manager state
coherent, and the trucks have to share a world to see each other at all.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import fcntl
import json
import logging
import os
import signal
import sys
import tempfile
from pathlib import Path
from typing import IO

import moq

from .bridge import Bridge, Rendition
from .runtime import TruckSession
from .watchdog import Watchdog
from .world import CameraConfig, Clock, World, bridge_stream

log = logging.getLogger("moq_carla_sim")

# The trucks this worker runs without `--truck`, which is how
# carla-worker.service runs it: the one list of trucks the demo is expected to
# show.
TRUCKS_FILE = Path(__file__).resolve().parent.parent / "trucks.json"

# A local development relay, which needs no token.
DEFAULT_URL = "http://localhost:4443"


def default_trucks() -> list[str]:
    trucks = json.loads(TRUCKS_FILE.read_text())
    if not isinstance(trucks, list) or not trucks or not all(isinstance(truck, str) and truck for truck in trucks):
        raise ValueError(f"{TRUCKS_FILE} must be a non-empty list of truck names")
    return trucks


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="moq-carla-sim", description=__doc__)
    parser.add_argument(
        "--url",
        default=os.environ.get("MOQ_URL", DEFAULT_URL),
        # A hosted URL carries its JWT in the query, so prefer MOQ_URL over the
        # flag: a command line is visible to every user on the host.
        help=f"relay URL, with ?jwt=... when hosted (default: MOQ_URL, else {DEFAULT_URL})",
    )
    parser.add_argument(
        "--truck",
        action="append",
        dest="trucks",
        default=None,
        help="truck name; repeat for more than one (default: the list in trucks.json)",
    )
    parser.add_argument("--camera", default="front", help="camera name in the broadcast path")
    parser.add_argument("--location", default=None, help="human-readable label echoed in telemetry")
    parser.add_argument("--carla-host", default="127.0.0.1")
    parser.add_argument("--carla-port", type=int, default=2000)
    parser.add_argument("--carla-timeout", type=float, default=30.0)
    parser.add_argument("--traffic-manager-port", type=int, default=8000)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument(
        "--bridge-socket",
        default=os.environ.get("CARLA_BRIDGE_SOCKET") or default_bridge_socket(),
        help="the simulator's GPU video bridge (default: CARLA_BRIDGE_SOCKET, or carla-bridge/bridge.sock "
        "under XDG_RUNTIME_DIR, where server.sh puts it)",
    )
    parser.add_argument("--log-level", default="info")
    args = parser.parse_args(argv)
    if not args.trucks:
        args.trucks = default_trucks()
    return args


def default_bridge_socket() -> str:
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    return str(Path(runtime_dir) / "carla-bridge" / "bridge.sock")


def renditions(camera: CameraConfig) -> list[Rendition]:
    return [
        Rendition(rendition.name, rendition.width, rendition.height, rendition.bitrate, rendition.fps)
        for rendition in camera.renditions()
    ]


async def connect_bridge(path: str) -> Bridge:
    """Connect to the bridge the simulator started when the cameras spawned."""
    try:
        return await Bridge.connect(path)
    except (FileNotFoundError, ConnectionRefusedError) as err:
        raise RuntimeError(
            f"no GPU video bridge at {path}: the cameras spawned, so is the simulator running with "
            "-moq-bridge-socket at that path (`just carla server`)?"
        ) from err


def broadcast_path(truck: str, camera: str) -> str:
    return f"carla/{truck}/{camera}.hang"


def driver_prefix(truck: str) -> str:
    """Where a truck's drivers publish, each at `{prefix}/{driverId}`."""
    return f"viewer/carla/{truck}"


# The one task allowed to end the run without failing it.
SHUTDOWN = "shutdown"


def claim_simulator(host: str, port: int) -> IO[str]:
    """Hold a lock naming this simulator for as long as the returned file is open.

    One worker owns the world: `sweep` removes every vehicle in it, so a second
    worker on the same simulator would destroy the first one's trucks, and the
    first would then fail and restart into the same sweep. A manual `just carla
    worker` beside the systemd unit is the usual way to get there. The lock is
    per host so it covers only that case; it says nothing about a worker on
    another machine, and neither does anything else here.
    """
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    path = Path(runtime_dir) / f"moq-carla-{host}-{port}.lock"
    lock = path.open("a+")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        raise RuntimeError(
            f"another worker already owns the simulator at {host}:{port} (lock {path}); "
            "stop it first, it would otherwise lose its trucks to this one's sweep"
        ) from None
    return lock


def terminate(_signum: int, _frame: object) -> None:
    """Turn SIGTERM into the KeyboardInterrupt SIGINT already is.

    Until `supervise` installs the loop's own handlers, the run is in blocking
    CARLA calls: connecting, sweeping, spawning. Python's default SIGTERM
    action ends the process there with no `finally`, leaving the freshly
    spawned trucks in the world for the next run to sweep. Raising instead
    unwinds through `run`'s cleanup, the same way Ctrl-C does.
    """
    raise KeyboardInterrupt


async def session_closed(client: moq.Client) -> None:
    """Raise once the relay session is over.

    `Session.closed()` resolves when the connection stops for good, by raising
    its error or by returning after a local shutdown, which nothing here
    performs while the trucks are running. Either way the broadcasts are gone
    from the relay and the run has to end.
    """
    session = client.session
    if session is None:
        raise RuntimeError("relay session is not connected")
    await session.closed()
    raise RuntimeError("relay session closed")


async def supervise(client: moq.Client, tasks: list[asyncio.Task[None]], clock: Clock, watchdog: Watchdog) -> None:
    """Run until a truck stops, the relay session or the clock dies, or a signal arrives.

    The session is watched alongside the trucks because its death is invisible
    from inside them: their tracks keep accepting frames, so the process goes on
    looking healthy while the relay announces nothing, and systemd has no
    failure to restart. Only raising gets us out of that state. The clock is
    the same shape of failure: a world nobody ticks is frozen, and every truck
    keeps looking healthy while publishing nothing.

    All of that assumes this loop still turns. The watchdog's pulse is what
    proves it does; see `watchdog` for the hang it exists to catch.
    """
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)

    watchers = [
        asyncio.create_task(stop.wait(), name=SHUTDOWN),
        asyncio.create_task(session_closed(client), name="session"),
        asyncio.create_task(clock.wait_failed(), name="clock"),
        asyncio.create_task(watchdog.pulse(), name="pulse"),
    ]

    done, pending = await asyncio.wait([*tasks, *watchers], return_when=asyncio.FIRST_COMPLETED)
    # Awaited, not just cancelled: each session releases its driver watcher on
    # the way out, and an un-awaited cancel would leave that to loop teardown,
    # after the world in `run` is already gone.
    for task in pending:
        task.cancel()
    await asyncio.gather(*pending, return_exceptions=True)
    # Surface whatever ended the process rather than exiting cleanly on a truck
    # or a session that died: neither must read as a normal stop.
    for task in done:
        if task.get_name() != SHUTDOWN and not task.cancelled():
            task.result()


async def run(args: argparse.Namespace) -> None:
    camera = CameraConfig(width=args.width, height=args.height, fps=args.fps)

    lock = claim_simulator(args.carla_host, args.carla_port)
    world = World.connect(args.carla_host, args.carla_port, args.carla_timeout, args.traffic_manager_port, camera.fps)
    sessions: list[TruckSession] = []
    # The RPC timeout bounds every blocking CARLA call, so a thread that has
    # not moved in twice that long is hung, not slow. The clock idles a frame
    # period between ticks on purpose, so that is not counted against it.
    watchdog = Watchdog(2 * args.carla_timeout + 1 / camera.fps, world.clock.heartbeat)
    # Spawning is inside the try: a spawn that fails partway has already put
    # actors in the world, and the server outlives us, so they must still be
    # removed or the next run spawns beside them. The clock starts first
    # because a synchronous sweep and spawn only land on a tick.
    try:
        world.clock.start()
        watchdog.start()
        world.sweep()
        world.spawn(args.trucks, camera, args.camera)
        map_name = world.map_name()
        # Every truck is in the world, so the clock no longer needs to run for
        # its own sake: from here it follows viewer and driver demand.
        world.ready()
        bridge = await connect_bridge(args.bridge_socket)
        # The only reader of the bridge's events, so it runs before any stream
        # opens. It ending, for any reason, ends the run: every truck's video
        # shares the connection.
        events = asyncio.create_task(bridge.run(), name="bridge")
        try:
            async with moq.connect(args.url) as client:
                try:
                    # One epoch for the run: a restarted worker announces every
                    # truck under a new one, which viewers see as a restart.
                    route = moq.AnnounceRoute(epoch=moq.mint_epoch())
                    for truck in world.trucks:
                        publication = client.create_broadcast(broadcast_path(truck.name, args.camera))
                        video = await bridge.open(
                            bridge_stream(truck.name, args.camera), renditions(camera), publication.broadcast
                        )
                        session = TruckSession(truck, publication, video, args.location, map_name, world.clock, route)
                        sessions.append(session)
                        log.info(
                            "publishing %s, consuming %s/*",
                            broadcast_path(truck.name, args.camera),
                            driver_prefix(truck.name),
                        )

                    tasks = [
                        asyncio.create_task(
                            session.run(client, driver_prefix(truck.name)),
                            name=f"truck:{truck.name}",
                        )
                        for session, truck in zip(sessions, world.trucks)
                    ]

                    await supervise(client, [*tasks, events], world.clock, watchdog)
                finally:
                    # Inside the session, so the tracks end cleanly for
                    # subscribers rather than with the connection.
                    for session in sessions:
                        session.stop_video()
                    # Also inside the session, ahead of its close: the actors are
                    # the one thing that outlives this process, so they go before
                    # anything that could hang on a relay that has already gone,
                    # and before systemd's stop timeout turns that hang into a kill.
                    world.destroy()
        finally:
            events.cancel()
            await asyncio.gather(events, return_exceptions=True)
            bridge.disconnect()
    finally:
        # A no-op on the way out of a completed run; this is for a sweep, a
        # spawn, or a connect that failed before the session was ever entered.
        world.destroy()
        # Armed through both destroys: a teardown that hangs is the same hang,
        # and after a failure nothing but the watchdog would end it.
        watchdog.stop()
        lock.close()


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    logging.basicConfig(
        level=args.log_level.upper(),
        format="%(asctime)s %(levelname)-5s %(name)s: %(message)s",
    )
    # The bindings log from Rust, which Python logging does not see; keep both
    # layers at the same level.
    moq.log_level(args.log_level.lower())
    signal.signal(signal.SIGTERM, terminate)
    with contextlib.suppress(asyncio.CancelledError, KeyboardInterrupt):
        asyncio.run(run(args))
    return 0


if __name__ == "__main__":
    sys.exit(main())
