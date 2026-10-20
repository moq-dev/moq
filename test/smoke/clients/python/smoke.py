"""Cross-language interop client for the smoke test (PyPI `moq-rs`, import `moq`).

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


async def publish(url: str, broadcast: str) -> None:
    async with moq.Client(url, tls_verify=False) as client:
        # create_broadcast registers the broadcast and hands back its producer
        # (the old client.publish(broadcast, producer) split was removed in the
        # ergonomic moq-rs API). publish_video_stream feeds a raw Annex-B pipe to
        # the streaming importer, which infers frame boundaries.
        producer = client.create_broadcast(broadcast)
        media = producer.publish_video_stream(moq.VideoFormat.AVC3)
        # create_broadcast only registers the path locally; announce advertises it
        # to the relay, once its tracks exist.
        producer.announce()
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
    # These live consumers are async context managers. Cancel them explicitly
    # before the parent client shuts down; otherwise an in-flight FFI task can
    # race teardown after this helper returns from the async iterator.
    async with await consumer.subscribe_catalog() as catalog_consumer:
        async for catalog in catalog_consumer:
            if catalog.video:
                return catalog
    raise RuntimeError("catalog stream ended without a video track")


async def subscribe(url: str, broadcast: str, timeout: float) -> None:
    async with moq.Client(url, tls_verify=False) as client:
        # announced_broadcast yields a handle; available() resolves it to a live
        # consumer once the broadcast is announced. subscribe_catalog/_media are
        # coroutines in the ergonomic API, so they're awaited (below and above).
        async with client.announced_broadcast(broadcast) as announced:
            consumer = await asyncio.wait_for(announced.available(), timeout)
        catalog = await asyncio.wait_for(_catalog_with_video(consumer), timeout)

        track_name = next(iter(catalog.video))
        video = catalog.video[track_name]

        total = 0
        async with await consumer.subscribe_media(track_name, video.container) as media:

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
