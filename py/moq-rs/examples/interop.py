"""Cross-language interop client for the smoke test.

publish:   replay an H.264 asset (see demo/smoke/gen-asset.sh) via
           publish_media("avc3", init) / write_frame, looping forever.
subscribe: connect, find the video track in the catalog, and exit 0 as soon as
           any non-empty frame arrives (exit 1 on timeout / no data).

    python interop.py publish   --url http://localhost:4443 --broadcast b.hang --asset DIR
    python interop.py subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
"""

import argparse
import asyncio
import json
import sys
from pathlib import Path

import moq

DEFAULT_FPS = 30
MICROSECONDS_PER_SECOND = 1_000_000
MAX_LATENCY_MS = 1_000  # subscribe_media congestion-control / lookahead window


async def publish(url: str, broadcast: str, asset_dir: str) -> None:
    root = Path(asset_dir)
    meta = json.loads((root / "asset.json").read_text())
    init = (root / meta["init_file"]).read_bytes()
    frames = [(root / f["file"]).read_bytes() for f in meta["frames"]]
    timestamps = [int(f["ts_us"]) for f in meta["frames"]]
    fps = int(meta.get("fps", DEFAULT_FPS))
    frame_dt = 1.0 / fps
    loop_us = timestamps[-1] + MICROSECONDS_PER_SECOND // fps

    producer = moq.BroadcastProducer()
    media = producer.publish_media(meta["format"], init)

    async with moq.Client(url, tls_verify=False) as client:
        client.publish(broadcast, producer)
        print(f"publishing {broadcast!r} ({len(frames)} frames) to {url}")

        base = 0
        while True:
            for payload, ts in zip(frames, timestamps, strict=True):
                media.write_frame(payload, base + ts)
                await asyncio.sleep(frame_dt)
            base += loop_us


async def subscribe(url: str, broadcast: str, timeout: float) -> None:
    async with moq.Client(url, tls_verify=False) as client:
        consumer = await asyncio.wait_for(client.announced_broadcast(broadcast), timeout)
        catalog = await asyncio.wait_for(consumer.catalog(), timeout)

        if not catalog.video:
            raise RuntimeError("catalog has no video track")
        track_name = next(iter(catalog.video))
        video = catalog.video[track_name]

        media = consumer.subscribe_media(track_name, video.container, MAX_LATENCY_MS)

        total = 0

        async def drain() -> None:
            nonlocal total
            async for frame in media:
                total += len(frame.payload)
                if total > 0:
                    return

        await asyncio.wait_for(drain(), timeout)

    if total <= 0:
        raise RuntimeError("no frame data received")
    print(f"received {total} bytes from {broadcast!r}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("role", choices=["publish", "subscribe"])
    parser.add_argument("--url", required=True)
    parser.add_argument("--broadcast", required=True)
    parser.add_argument("--asset", help="asset dir (publish only)")
    parser.add_argument("--timeout", type=float, default=20.0)
    args = parser.parse_args()

    try:
        if args.role == "publish":
            if not args.asset:
                parser.error("--asset is required for publish")
            asyncio.run(publish(args.url, args.broadcast, args.asset))
        else:
            asyncio.run(subscribe(args.url, args.broadcast, args.timeout))
    except KeyboardInterrupt:
        pass
    except (TimeoutError, asyncio.TimeoutError):
        print("error: timed out waiting for data", file=sys.stderr)
        sys.exit(1)
    except Exception as err:  # noqa: BLE001 - smoke client: any failure is a failure
        print(f"error: {err}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
