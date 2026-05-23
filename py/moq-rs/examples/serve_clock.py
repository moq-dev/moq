"""Serve a clock broadcast from a local MoQ server.

Generates a self-signed certificate for `localhost`, binds the server,
and publishes a "clock" broadcast where each minute is a group and each
second is a frame, in the same format as `examples/clock.py`.

Run a subscriber against it with TLS verification disabled, e.g.:

    python py/moq-rs/examples/serve_clock.py --bind 127.0.0.1:4443
    python py/moq-rs/examples/clock.py subscribe \\
        --url https://127.0.0.1:4443 --broadcast clock --no-tls-verify
"""

import argparse
import asyncio
from datetime import datetime, timezone

import moq


async def run(bind: str, broadcast_name: str, track_name: str, host: str) -> None:
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track(track_name)

    async with moq.Server(bind, tls_generate=[host]) as server:
        server.publish(broadcast_name, broadcast)
        print(f"serving {broadcast_name!r} track={track_name!r} on https://{server.local_addr}")
        for fp in server.cert_fingerprints():
            print(f"  cert fingerprint sha256: {fp}")

        serve_task = asyncio.create_task(server.serve())
        try:
            while True:
                now = datetime.now(timezone.utc).replace(microsecond=0)
                group = track.append_group()
                group.write_frame(now.strftime("%Y-%m-%d %H:%M:").encode())

                current_minute = now.minute
                while now.minute == current_minute:
                    group.write_frame(now.strftime("%S").encode())
                    await asyncio.sleep(1 - datetime.now(timezone.utc).microsecond / 1_000_000)
                    now = datetime.now(timezone.utc).replace(microsecond=0)

                group.finish()
        finally:
            serve_task.cancel()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1:4443", help="address to bind (default: 127.0.0.1:4443)")
    parser.add_argument("--broadcast", default="clock", help="broadcast path (default: clock)")
    parser.add_argument("--track", default="seconds", help="track name (default: seconds)")
    parser.add_argument("--host", default="localhost", help="hostname for the self-signed cert (default: localhost)")
    args = parser.parse_args()

    try:
        asyncio.run(run(args.bind, args.broadcast, args.track, args.host))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
