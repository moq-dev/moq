"""Cross-language interop client for the smoke test (workspace py/moq-rs, import `moq`).

publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
           it to a streaming importer, which infers frame boundaries. Alongside
           it, encode a synthetic tone through libopus so the matrix exercises
           the FFI audio path, not only the video one.
subscribe: connect, find the video track in the catalog, and exit 0 as soon as
           any non-empty frame arrives (exit 1 on timeout / no data).

    ffmpeg ... -f h264 - | python smoke.py publish --url http://localhost:4443 --broadcast b.hang
    python smoke.py subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20
"""

import argparse
import asyncio
import contextlib
import math
import struct
import sys

import moq

READ_CHUNK = 64 * 1024
MAX_AGE_US = 1_000_000  # subscribe_media congestion-control / lookahead window

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


async def _publish_tone(audio: moq.AudioProducer) -> None:
    """Feed the encoder a real-time tone until the task is cancelled."""
    timestamp_us = 0
    phase = 0
    loop = asyncio.get_running_loop()
    started = loop.time()
    while True:
        samples = [math.sin(2 * math.pi * AUDIO_TONE_HZ * (phase + i) / AUDIO_RATE) for i in range(AUDIO_BATCH_SAMPLES)]
        audio.write(moq.AudioFrame(timestamp_us=timestamp_us, data=struct.pack(f"<{len(samples)}f", *samples)))
        phase += AUDIO_BATCH_SAMPLES
        timestamp_us += AUDIO_BATCH_US
        # Pace against the start, so encoding cost doesn't accumulate as drift.
        await asyncio.sleep(max(0.0, started + timestamp_us / 1_000_000 - loop.time()))


async def publish(url: str, broadcast: str) -> None:
    async with moq.Client(url, tls_verify=False) as client:
        # Hold the producer for the lifetime of the publish loop; finish() unpublishes.
        producer = client.create_broadcast(broadcast)
        media = producer.publish_video_stream(moq.VideoFormat.AVC3)
        audio = producer.encode_audio(
            AUDIO_TRACK,
            moq.AudioEncoderInput(format=moq.AudioSampleFormat.F32, sample_rate=AUDIO_RATE, channels=1),
            moq.AudioEncoderOutput(codec=moq.AudioCodec.opus(), frame_duration_us=AUDIO_FRAME_DURATION_US),
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

        media = await consumer.subscribe_media(
            track_name, video.container, moq.Subscription(max_age_us=MAX_AGE_US)
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
