"""Park the renderer of a simulator no worker owns.

The server outlives every client and boots with rendering on, so a simulator
sitting between workers draws frames nobody asked for. Measured on an
RTX 3070 Ti against an empty world with nothing connected: 174 W and 100% GPU
before, 26 W and 44% after, with process CPU down about half a core. The worker
asserts the same state on connect (see `World.__init__`); this is that
assertion for the gap before one arrives.

Only `no_rendering_mode`. Synchronous mode is deliberately untouched: a
synchronous world nobody ticks is frozen for whatever connects next, and
`just carla benchmark` refuses a world that is already synchronous.
"""

from __future__ import annotations

import argparse
import logging
import sys

from .main import claim_simulator
from .world import connect_client

log = logging.getLogger(__name__)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="carla-park", description=__doc__)
    parser.add_argument("--carla-host", default="127.0.0.1")
    parser.add_argument("--carla-port", type=int, default=2000)
    parser.add_argument("--carla-timeout", type=float, default=30.0)
    parser.add_argument("--log-level", default="info")
    return parser.parse_args(argv)


def park(host: str, port: int, timeout: float) -> bool:
    """Disable rendering on an unowned simulator. True when it parked one.

    The worker's own lock decides ownership: a worker holding it is already
    driving demand, and parking under it would black out a live viewer between
    its `select_camera` and the sensor's next frame.
    """
    try:
        lock = claim_simulator(host, port)
    except RuntimeError:
        log.info("a worker owns %s:%d; leaving its rendering alone", host, port)
        return False

    try:
        world = connect_client(host, port, timeout).get_world()
        settings = world.get_settings()
        if settings.no_rendering_mode:
            log.info("rendering already parked")
            return True
        settings.no_rendering_mode = True
        world.apply_settings(settings)
        log.info("rendering parked until a worker or a viewer needs it")
        return True
    finally:
        lock.close()


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    logging.basicConfig(
        level=args.log_level.upper(),
        format="%(asctime)s %(levelname)-5s %(name)s: %(message)s",
    )
    park(args.carla_host, args.carla_port, args.carla_timeout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
