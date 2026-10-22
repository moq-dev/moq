"""Cross-language interop client for the interop test (workspace py/moq-rs, import `moq`).

publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
           it to a streaming importer, which infers frame boundaries. Alongside
           it, encode a tone per channel (--tone) through libopus so the matrix
           exercises the FFI audio path, not only the video one.
subscribe: connect, wait for a catalog with video and audio, and exit 0 once a
           non-empty video frame has arrived and --audio-ms of audio has been
           decoded to --audio-out for tone.py to judge (exit 1 on timeout / no data).

--version offers only that version. --grant waits for the grant the relay sends over
AUTH and prints it as an `auth granted` line before the client starts.

    ffmpeg ... -f h264 - | python interop.py publish --url http://localhost:4443 --broadcast b.hang --tone 440
    python interop.py subscribe --url http://localhost:4443 --broadcast b.hang --timeout 20 \
        --audio-out /tmp/audio --audio-ms 600
"""

import argparse
import asyncio
import contextlib
import json
import math
import struct
import sys
from datetime import timedelta

import moq

READ_CHUNK = 64 * 1024
MAX_DELAY = timedelta(seconds=1)  # container consumer congestion-control / lookahead window

# Synthetic audio: a 48 kHz tone per channel at 0.5 amplitude, encoded as Opus.
AUDIO_TRACK = "tone"
AUDIO_RATE = 48_000
AUDIO_AMPLITUDE = 0.5
# A non-default frame duration, and the shortest Opus offers. 20 ms would pass
# even if the microsecond field were truncated to milliseconds somewhere.
AUDIO_FRAME_DURATION_US = 2_500
# Written in 20 ms batches, so each write spans eight encoded Opus frames.
AUDIO_BATCH_US = 20_000
AUDIO_BATCH_SAMPLES = AUDIO_RATE * AUDIO_BATCH_US // 1_000_000


async def _publish_tone(audio: moq.audio.Producer, tones: list[int]) -> None:
    """Feed the encoder a real-time tone per channel, interleaved, until the task is cancelled."""
    timestamp_us = 0
    phase = 0
    loop = asyncio.get_running_loop()
    started = loop.time()
    while True:
        samples = [
            AUDIO_AMPLITUDE * math.sin(2 * math.pi * hz * (phase + i) / AUDIO_RATE)
            for i in range(AUDIO_BATCH_SAMPLES)
            for hz in tones
        ]
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
    url: str, broadcast: str, version: str | None, grant: bool, timeout: float, tones: list[int]
) -> None:
    async with _client(url, version) as client:
        if grant:
            await _print_grant(client, timeout)
        # Hold the producer for the lifetime of the publish loop; finish() unpublishes.
        producer = client.create_broadcast(broadcast)
        catalog = moq.media.CatalogProducer(producer)
        media = moq.media.TrackStreamProducer.video(
            producer, moq.media.VideoInit(format=moq.media.VideoFormat.AVC3, data=b"")
        )
        audio = moq.audio.Producer(
            catalog,
            moq.audio.EncoderInput(format=moq.audio.SampleFormat.F32, sample_rate=AUDIO_RATE, channels=len(tones)),
            moq.audio.EncoderOutput(
                codec=moq.audio.Codec.opus(),
                track=AUDIO_TRACK,
                frame_duration=timedelta(microseconds=AUDIO_FRAME_DURATION_US),
            ),
        )
        producer.announce()
        print(f"publishing {broadcast!r} (Annex-B H.264 from stdin + a {tones} Hz tone) to {url}")

        tone = asyncio.create_task(_publish_tone(audio, tones))
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


async def _catalog_with_media(consumer: moq.BroadcastConsumer) -> moq.media.Catalog:
    # The catalog is a live track. A lazy publisher (e.g. the browser, which only
    # encodes on demand) may announce a rendition in a *later* update, not the first
    # snapshot, so wait for a catalog that has both video and audio.
    catalog_consumer = await moq.media.CatalogConsumer.subscribe(consumer)
    async for catalog in catalog_consumer:
        if catalog.video and catalog.audio:
            return catalog
    raise RuntimeError("catalog stream ended without a video and an audio track")


async def _decode_audio(
    consumer: moq.BroadcastConsumer, id: str, audio: moq.media.Audio, out: str, audio_ms: int
) -> None:
    """Decode the first `audio_ms` of the rendition to `out`.{txt,pcm}, the layout tone.py reads."""
    decoder = await moq.audio.Consumer.subscribe(
        consumer, id, audio, moq.audio.DecoderOutput(format=moq.audio.SampleFormat.F32, max_delay=MAX_DELAY)
    )
    frame_bytes = 4 * audio.channel_count
    decoded = 0
    with open(f"{out}.txt", "w") as log, open(f"{out}.pcm", "wb") as pcm:
        log.write(f"{audio.sample_rate} {audio.channel_count}\n")
        async for frame in decoder:
            samples = len(frame.data) // frame_bytes
            log.write(f"{frame.timestamp // timedelta(microseconds=1)} {samples}\n")
            pcm.write(frame.data)
            decoded += samples
            if decoded * 1000 >= audio_ms * audio.sample_rate:
                decoder.cancel()
                return
    raise RuntimeError("audio track ended early")


async def subscribe(
    url: str, broadcast: str, version: str | None, grant: bool, timeout: float, audio_out: str, audio_ms: int
) -> None:
    async with _client(url, version) as client:
        if grant:
            await _print_grant(client, timeout)
        consumer = await asyncio.wait_for(client.announced_broadcast(broadcast), timeout)
        catalog = await asyncio.wait_for(_catalog_with_media(consumer), timeout)

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

        audio_id, audio = next(iter(catalog.audio.items()))
        await asyncio.wait_for(
            asyncio.gather(drain(), _decode_audio(consumer, audio_id, audio, audio_out, audio_ms)), timeout
        )

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
    parser.add_argument("--tone", help="publish: comma-separated frequency of each channel, in Hz")
    parser.add_argument("--audio-out", help="subscribe: decode audio to this prefix's .txt and .pcm")
    parser.add_argument("--audio-ms", type=int, help="subscribe: how much audio to decode")
    args = parser.parse_args()
    if args.role == "publish" and not args.tone:
        parser.error("publish needs --tone")
    if args.role == "subscribe" and (not args.audio_out or not args.audio_ms):
        parser.error("subscribe needs --audio-out and --audio-ms")

    # Warnings and errors from the Rust layer, such as a publication outside the grant.
    moq.log_level("warn")
    try:
        if args.role == "publish":
            tones = [int(hz) for hz in args.tone.split(",")]
            asyncio.run(publish(args.url, args.broadcast, args.version, args.grant, args.timeout, tones))
        else:
            asyncio.run(
                subscribe(
                    args.url, args.broadcast, args.version, args.grant, args.timeout, args.audio_out, args.audio_ms
                )
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
