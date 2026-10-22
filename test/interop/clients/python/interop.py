"""Cross-language interop client for the interop test (workspace py/moq-rs, import `moq`).

publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
           it to a streaming importer, which infers frame boundaries. Alongside
           it, encode a synthetic tone through libopus so the matrix exercises
           the FFI audio path, not only the video one.
subscribe: connect, find the video track in the catalog, and exit 0 as soon as
           any non-empty frame arrives (exit 1 on timeout / no data).

--finish verifies a finite tail, natural media/catalog EOF, announcement END, and
same-session health; --frames sets its exact size. --finish-ack selects publisher
finish mode and names the FIFO releasing teardown after the real linger.
--version offers only that version. --grant waits for the grant the relay sends over
AUTH and prints it as an `auth granted` line before the client starts.

    ffmpeg ... -f h264 - | python interop.py publish --url http://localhost:4443 --broadcast b.hang
    python interop.py subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
"""

import argparse
import asyncio
import contextlib
import json
import math
import struct
import sys
import time
from datetime import timedelta

import moq

READ_CHUNK = 64 * 1024
MAX_DELAY = timedelta(seconds=1)  # container consumer congestion-control / lookahead window

# Synthetic audio: a 48 kHz mono tone, encoded as Opus.
AUDIO_TRACK = "tone"
AUDIO_RATE = 48_000
AUDIO_TONE_HZ = 440.0
# A non-default frame duration, and the shortest Opus offers. 20 ms would pass
# even if the microsecond field were truncated to milliseconds somewhere.
AUDIO_FRAME_DURATION_US = 2_500
# Written in 20 ms batches, so each write spans eight encoded Opus frames.
AUDIO_BATCH_US = 20_000
AUDIO_BATCH_SAMPLES = AUDIO_RATE * AUDIO_BATCH_US // 1_000_000


async def _publish_tone(audio: moq.audio.Producer) -> None:
    """Feed the encoder a real-time tone until the task is cancelled."""
    timestamp_us = 0
    phase = 0
    loop = asyncio.get_running_loop()
    started = loop.time()
    while True:
        samples = [math.sin(2 * math.pi * AUDIO_TONE_HZ * (phase + i) / AUDIO_RATE) for i in range(AUDIO_BATCH_SAMPLES)]
        audio.write(
            moq.audio.Frame(
                timestamp=timedelta(microseconds=timestamp_us), data=struct.pack(f"<{len(samples)}f", *samples)
            )
        )
        phase += AUDIO_BATCH_SAMPLES
        timestamp_us += AUDIO_BATCH_US
        # Pace against the start, so encoding cost doesn't accumulate as drift.
        await asyncio.sleep(max(0.0, started + timestamp_us / 1_000_000 - loop.time()))


def _client(url: str, version: str | None) -> moq.Client:
    return moq.Client(url, tls_verify=False, versions=[] if version is None else [version])


async def _print_grant(client: moq.Client, timeout: float) -> None:
    """Wait for the relay's grant and print it, in the Rust client's `auth granted` shape."""
    session = client.session
    assert session is not None
    auth = session.auth()

    async def first() -> moq.Grant:
        while (grant := auth.grant()) is None:
            await auth.grant_changed()
        return grant

    grant = await asyncio.wait_for(first(), timeout)
    print(
        f"auth granted publish={json.dumps(list(grant.publish))} subscribe={json.dumps(list(grant.subscribe))}",
        file=sys.stderr,
    )


async def publish(
    url: str, broadcast: str, version: str | None, grant: bool, timeout: float, finish_ack: str | None
) -> None:
    async with _client(url, version) as client:
        if grant:
            await _print_grant(client, timeout)
        # Hold the producer for the lifetime of the publish loop.
        producer = client.create_broadcast(broadcast)
        catalog = moq.media.CatalogProducer(producer)
        media = moq.media.TrackStreamProducer.video(
            producer, moq.media.VideoInit(format=moq.media.VideoFormat.AVC3, data=b"")
        )
        audio = moq.audio.Producer(
            catalog,
            moq.audio.EncoderInput(format=moq.audio.SampleFormat.F32, sample_rate=AUDIO_RATE, channels=1),
            moq.audio.EncoderOutput(
                codec=moq.audio.Codec.opus(),
                track=AUDIO_TRACK,
                frame_duration=timedelta(microseconds=AUDIO_FRAME_DURATION_US),
            ),
        )
        producer.announce()
        print(f"publishing {broadcast!r} (Annex-B H.264 from stdin + a {AUDIO_TONE_HZ:.0f} Hz tone) to {url}")

        tone = asyncio.create_task(_publish_tone(audio))
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
        tone.cancel()
        # Let the tone unwind before finishing, so no write races finish().
        with contextlib.suppress(asyncio.CancelledError):
            await tone
        audio.finish()
        media.finish()
        if finish_ack is not None:
            # finish() also finalizes the catalog. ACK never controls the real linger.
            started = time.monotonic_ns()
            finished = producer.finish()
            linger_us = finished.linger // timedelta(microseconds=1)
            if linger_us <= 0:
                raise RuntimeError("finished media has no linger")
            print(f"finish ready linger_us={linger_us}", flush=True)
            await finished.wait()
            if time.monotonic_ns() - started < linger_us * 1000:
                raise RuntimeError("finish wait ended before its linger")
            health = client.create_broadcast(f"{broadcast}.health")
            live = health.publish_track("live")
            live.write_frame(b"session-alive")
            live.finish()
            health.announce()

            def acknowledge() -> None:
                with open(finish_ack) as ack:
                    if ack.readline() != "complete\n":
                        raise RuntimeError("expected finish completion acknowledgement")

            await loop.run_in_executor(None, acknowledge)
            if client.session is None or client.session.connects() != 1:
                raise RuntimeError("publisher reconnected during finish")
            print("finish acknowledged", flush=True)


async def _catalog_with_video(consumer: moq.BroadcastConsumer) -> moq.media.Catalog:
    # The catalog is a live track. A lazy publisher (e.g. the browser, which only
    # encodes on demand) may announce video in a *later* update, not the first
    # snapshot, so wait for a catalog that actually has a video track.
    catalog_consumer = await moq.media.CatalogConsumer.subscribe(consumer)
    async for catalog in catalog_consumer:
        if catalog.video:
            return catalog
    raise RuntimeError("catalog stream ended without a video track")


async def _finish_subscribe(client: moq.Client, broadcast: str, frames: int) -> None:
    # Watch the exact path before resolving it, retaining END while media drains.
    async with client.announced(broadcast) as announcements:
        async with client.announced_broadcast(broadcast) as announced:
            consumer = await announced
            async with await moq.media.CatalogConsumer.subscribe(consumer) as catalogs:
                async for catalog in catalogs:
                    if catalog.video:
                        break
                else:
                    raise RuntimeError("catalog ended without video")
                id, video = next(iter(catalog.video.items()))
                async with await moq.media.TrackConsumer.subscribe(
                    consumer, id, video, subscription=moq.Subscription(max_delay=MAX_DELAY)
                ) as media:
                    count = 0
                    async for frame in media:
                        # Empty payloads carry endpoint metadata, not codec frames.
                        if not frame.payload:
                            continue
                        count += 1
                        if count > frames:
                            raise RuntimeError(f"expected {frames} frames, received more")
                    if count != frames:
                        raise RuntimeError(f"expected {frames} frames, received {count}")
                # Do not cancel the catalog after its first snapshot: EOF is part of the case.
                async for _ in catalogs:
                    pass

        started = False
        async for event in announcements:
            if isinstance(event, moq.AnnounceEventRestart) and event.announce.prefix == broadcast:
                raise RuntimeError("broadcast restarted during finish")
            if isinstance(event, moq.AnnounceEventStart) and event.announce.prefix == broadcast:
                started = True
            if isinstance(event, moq.AnnounceEventEnd) and event.announce.prefix == broadcast:
                if not started:
                    raise RuntimeError("broadcast ended without announcement")
                break
        else:
            raise RuntimeError("announcement stream ended before broadcast END")

    async with client.announced_broadcast(f"{broadcast}.health") as announced_health:
        health = await announced_health
        async with await health.subscribe_track("live") as live:
            seen = False
            async for group in live:
                async with group:
                    async for frame in group:
                        if seen or frame.payload != b"session-alive" or frame.timestamp is not None:
                            raise RuntimeError("unexpected session health payload or timestamp")
                        seen = True
            if not seen:
                raise RuntimeError("missing session health payload")
    if client.session is None or client.session.connects() != 1:
        raise RuntimeError("subscriber reconnected during finish")
    print(f"finish clean frames={frames} catalog=eof announce=end health=ok", flush=True)


async def subscribe(
    url: str, broadcast: str, version: str | None, grant: bool, timeout: float, finish: bool, frames: int
) -> None:
    async with _client(url, version) as client:
        if grant:
            await _print_grant(client, timeout)
        if finish:
            print("finish reader ready", flush=True)
            command = await asyncio.wait_for(asyncio.to_thread(sys.stdin.readline), timeout)
            if command != "read\n":
                raise RuntimeError("expected finish read command")
            await asyncio.wait_for(_finish_subscribe(client, broadcast, frames), timeout)
            return
        consumer = await asyncio.wait_for(client.announced_broadcast(broadcast), timeout)
        catalog = await asyncio.wait_for(_catalog_with_video(consumer), timeout)

        id, video = next(iter(catalog.video.items()))

        media = await moq.media.TrackConsumer.subscribe(
            consumer, id, video, subscription=moq.Subscription(max_delay=MAX_DELAY)
        )

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
    parser.add_argument("--version")
    parser.add_argument("--grant", action="store_true")
    parser.add_argument("--finish-ack", help="publisher completion FIFO; enables clean finish")
    parser.add_argument("--finish", action="store_true", help="verify a late tail and clean broadcast end")
    parser.add_argument("--frames", type=int, default=10, help="expected finish video frame count")
    args = parser.parse_args()
    if args.frames <= 0 or (args.finish and args.role != "subscribe") or (args.finish_ack and args.role != "publish"):
        parser.error("finish requires a subscriber, finish-ack a publisher, and frames must be positive")

    # Warnings and errors from the Rust layer, such as a publication outside the grant.
    moq.log_level("warn")
    try:
        if args.role == "publish":
            asyncio.run(publish(args.url, args.broadcast, args.version, args.grant, args.timeout, args.finish_ack))
        else:
            asyncio.run(
                subscribe(args.url, args.broadcast, args.version, args.grant, args.timeout, args.finish, args.frames)
            )
    except KeyboardInterrupt:
        pass
    except (TimeoutError, asyncio.TimeoutError):
        print("error: timed out waiting for data", file=sys.stderr)
        sys.exit(1)
    except Exception as err:  # noqa: BLE001 - interop client: any failure is a failure
        print(f"error: {err}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
