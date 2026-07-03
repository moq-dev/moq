"""Cross-language interop client for the smoke test (workspace py/moq-rs, import `moq`).

publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
           it to a streaming importer, which infers frame boundaries.
subscribe: connect, find the video track in the catalog, and exit 0 as soon as
           any non-empty frame arrives (exit 1 on timeout / no data).

    ffmpeg ... -f h264 - | python smoke.py publish --url http://localhost:4443 --broadcast b.hang
    python smoke.py subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
"""

import argparse
import asyncio
import sys

import moq

READ_CHUNK = 64 * 1024
MAX_LATENCY_MS = 1_000  # subscribe_media congestion-control / lookahead window


async def publish(url: str, broadcast: str) -> None:
    producer = moq.BroadcastProducer()
    media = producer.publish_media_stream("avc3")

    async with moq.Client(url, tls_verify=False) as client:
        client.publish(broadcast, producer)
        print(f"publishing {broadcast!r} (Annex-B H.264 from stdin) to {url}")

        loop = asyncio.get_running_loop()
        stdin = sys.stdin.buffer
        # read1 returns as soon as any bytes are available (read() would block
        # for a full chunk and batch up ffmpeg's real-time output). getattr both
        # keeps pyright happy (BinaryIO doesn't declare read1) and falls back if
        # the stream lacks it.
        read = getattr(stdin, "read1", stdin.read)
        while True:
            # Blocking read off the event loop so the client keeps flushing.
            chunk = await loop.run_in_executor(None, read, READ_CHUNK)
            if not chunk:
                break
            media.write(chunk)
        media.finish()


async def _catalog_with_video(consumer: moq.BroadcastConsumer) -> moq.Catalog:
    # The catalog is a live track. A lazy publisher (e.g. the browser, which only
    # encodes on demand) may announce video in a *later* update, not the first
    # snapshot, so wait for a catalog that actually has a video track.
    catalog_consumer = await consumer.subscribe_catalog()
    async for catalog in catalog_consumer:
        if catalog.video:
            return catalog
    raise RuntimeError("catalog stream ended without a video track")


async def subscribe(url: str, broadcast: str, timeout: float) -> None:
    async with moq.Client(url, tls_verify=False) as client:
        consumer = await asyncio.wait_for(client.announced_broadcast(broadcast), timeout)
        catalog = await asyncio.wait_for(_catalog_with_video(consumer), timeout)

        track_name = next(iter(catalog.video))
        video = catalog.video[track_name]

        media = await consumer.subscribe_media(track_name, video.container, MAX_LATENCY_MS)

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
    parser.add_argument("--timeout", type=float, default=20.0)
    args = parser.parse_args()

    try:
        if args.role == "publish":
            asyncio.run(publish(args.url, args.broadcast))
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
