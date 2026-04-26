"""Publish or subscribe to a clock track — the Python twin of `rs/moq-clock`.

Each minute is a new group; each second is a frame inside that group. The first
frame of every group is the "YYYY-MM-DD HH:MM:" prefix, followed by one "SS"
frame per second.

    python py/moq-lite/examples/clock.py publish   --url https://relay.example.com --broadcast clock
    python py/moq-lite/examples/clock.py subscribe --url https://relay.example.com --broadcast clock
"""

import argparse
import asyncio
from datetime import datetime, timezone

import moq_lite as moq


async def publish(url: str, broadcast_name: str, track_name: str, tls_verify: bool) -> None:
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track(track_name)

    async with moq.Client(url, tls_verify=tls_verify) as client:
        client.publish(broadcast_name, broadcast)
        print(f"publishing {broadcast_name!r} track={track_name!r} at {url}")

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


async def subscribe(url: str, broadcast_name: str, track_name: str, tls_verify: bool) -> None:
    async with moq.Client(url, tls_verify=tls_verify) as client:
        print(f"waiting for {broadcast_name!r} at {url}")
        broadcast = await client.announced_broadcast(broadcast_name)

        print(f"subscribed to {broadcast_name!r} track={track_name!r}")
        track = broadcast.subscribe_track(track_name)

        async for group in track:
            prefix: bytes | None = None
            async for frame in group:
                if prefix is None:
                    prefix = frame
                    continue
                print(f"{prefix.decode()}{frame.decode()}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("role", choices=["publish", "subscribe"], help="publish or subscribe")
    parser.add_argument("--url", required=True, help="relay URL (https://...)")
    parser.add_argument("--broadcast", default="clock", help="broadcast path (default: clock)")
    parser.add_argument("--track", default="seconds", help="track name (default: seconds)")
    parser.add_argument("--no-tls-verify", action="store_true", help="disable TLS verification (dev only)")
    args = parser.parse_args()

    runner = publish if args.role == "publish" else subscribe
    try:
        asyncio.run(runner(args.url, args.broadcast, args.track, tls_verify=not args.no_tls_verify))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
