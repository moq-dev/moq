"""Cross-language interop client for the interop test (workspace py/moq-rs, import `moq`).

publish:   read raw Annex-B H.264 from stdin (e.g. piped from ffmpeg) and feed
           it to a streaming importer, which infers frame boundaries. Alongside
           it, encode a tone per channel (--tone) through libopus so the matrix
           exercises the FFI audio path, not only the video one.
subscribe: connect, wait for a catalog with video and audio, and exit 0 once a
           non-empty video frame has arrived and --audio-ms of every audio rendition
           has been decoded to --audio-out.<codec> for tone.py to judge (exit 1 on
           timeout / no data).

--finish verifies a finite tail, natural media/catalog EOF, announcement END, and
same-session health; --frames sets its exact size. --finish-ack selects publisher
finish mode and names the FIFO releasing teardown after the real linger.
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
# How far a read may fall behind live before it skips. A gap fails the run, which a reader skipping
# behind live would cause itself: the matrix starts every subscriber at once, and a runtime starved
# by the others can fall a second behind. That is this reader's speed, not the delivery under test.
# A track's max-age caps it, so interop.sh's Rust publisher keeps its media at least this long.
MAX_DELAY = timedelta(seconds=5)
LINGER = timedelta(seconds=10)  # how long a finished publisher stays up for the late readers

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
    return moq.Client(url, tls=moq.ClientTls(insecure=True), versions=[] if version is None else [version])


async def _print_grant(client: moq.Client, timeout: float) -> None:
    """Wait for the relay's grant and print it, in the Rust client's `auth granted` shape."""
    session = client.session
    assert session is not None
    async def first() -> moq.Grant:
        async with session.grants() as grants:
            async for grant in grants:
                if grant is not None:
                    return grant
        raise RuntimeError("the session ended before a grant")

    grant = await asyncio.wait_for(first(), timeout)
    print(
        f"auth granted publish={json.dumps(list(grant.publish))} subscribe={json.dumps(list(grant.subscribe))}",
        file=sys.stderr,
    )


async def publish(
    url: str,
    broadcast: str,
    version: str | None,
    grant: bool,
    timeout: float,
    tones: list[int],
    finish_ack: str | None,
) -> None:
    async with _client(url, version) as client:
        if grant:
            await _print_grant(client, timeout)
        # Hold the producer for the lifetime of the publish loop.
        publication = client.create_broadcast(broadcast)
        producer = publication.broadcast
        catalog = moq.media.CatalogProducer(producer)
        media = moq.media.TrackStreamProducer.video(
            producer, moq.media.VideoInit(format=moq.media.VideoFormat.AVC3, data=b"")
        )
        audio = moq.audio.Producer(
            catalog,
            moq.audio.EncoderInput(format=moq.audio.SampleFormat.F32, sample_rate=AUDIO_RATE, channels=len(tones)),
            moq.audio.EncoderOutput(
                codec=moq.audio.Codec.opus(),
                frame_duration=timedelta(microseconds=AUDIO_FRAME_DURATION_US),
            ),
            track=AUDIO_TRACK,
        )
        publication.announce()
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
        if finish_ack is not None:
            # Finishing the catalog ends the content; the broadcast stays up for the late readers
            # until close() takes it offline. ACK never controls the linger.
            catalog.finish()
            print(f"finish ready linger_us={LINGER // timedelta(microseconds=1)}", flush=True)
            await asyncio.sleep(LINGER.total_seconds())
            producer.close()
            health = client.create_broadcast(f"{broadcast}.health")
            live = health.broadcast.publish_track("live")
            live.write_frame(b"session-alive")
            live.finish()
            health.announce()

            def acknowledge() -> None:
                with open(finish_ack) as ack:
                    if ack.readline() != "complete\n":
                        raise RuntimeError("expected finish completion acknowledgement")

            await loop.run_in_executor(None, acknowledge)
            if client.session is None or client.session.state().connects != 1:
                raise RuntimeError("publisher reconnected during finish")
            print("finish acknowledged", flush=True)


async def _catalog_with_media(consumer: moq.BroadcastConsumer) -> moq.media.Catalog:
    # The catalog is a live track. A lazy publisher (e.g. the browser, which only
    # encodes on demand) may announce a rendition in a *later* update, not the first
    # snapshot, so wait for a catalog that has both video and audio.
    catalog_consumer = await moq.media.CatalogConsumer.subscribe(consumer)
    async for catalog in catalog_consumer:
        if catalog.video and catalog.audio:
            return catalog
    raise RuntimeError("catalog stream ended without a video and an audio track")


async def _finish_subscribe(client: moq.Client, broadcast: str, frames: int) -> None:
    # Watch the exact path before resolving it, retaining END while media drains.
    async with client.announced(broadcast) as announcements:
        async with client.follow(broadcast) as announced:
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
                        if isinstance(frame, moq.media.Gap):
                            raise RuntimeError("media gap")
                        if isinstance(frame, moq.media.End):
                            continue
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

    async with client.follow(f"{broadcast}.health") as announced_health:
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
    if client.session is None or client.session.state().connects != 1:
        raise RuntimeError("subscriber reconnected during finish")
    print(f"finish clean frames={frames} catalog=eof announce=end health=ok", flush=True)


async def _decode_audio(
    consumer: moq.BroadcastConsumer, id: str, audio: moq.media.Audio, out: str, audio_ms: int
) -> None:
    """Decode the first `audio_ms` of the rendition to `out`.<codec>.{txt,pcm}, the layout tone.py reads.

    The codec is the catalog's up to its first `.` (`opus`, `mp4a`). The files are created
    exclusively, so two renditions of one codec fail rather than overwrite each other.
    """
    out = f"{out}.{audio.codec.split('.')[0]}"
    decoder = await moq.audio.Consumer.subscribe(
        consumer, id, audio, moq.audio.DecoderOutput(format=moq.audio.SampleFormat.F32, max_delay=MAX_DELAY)
    )
    frame_bytes = 4 * audio.channel_count
    decoded = 0
    with open(f"{out}.txt", "x") as log, open(f"{out}.pcm", "xb") as pcm:
        log.write(f"{audio.sample_rate} {audio.channel_count}\n")
        async for frame in decoder:
            # moq-audio reports a hole it saw directly; tone.py checks the timestamps too.
            if isinstance(frame, moq.audio.Gap):
                raise RuntimeError(f"audio gap after {decoded} samples")
            if isinstance(frame, moq.audio.End):
                raise RuntimeError("audio track ended early")
            samples = len(frame.data) // frame_bytes
            log.write(f"{frame.timestamp // timedelta(microseconds=1)} {samples}\n")
            pcm.write(frame.data)
            decoded += samples
            if decoded * 1000 >= audio_ms * audio.sample_rate:
                decoder.cancel()
                return
    raise RuntimeError("audio track ended early")


async def subscribe(
    url: str,
    broadcast: str,
    version: str | None,
    grant: bool,
    timeout: float,
    audio_out: str | None,
    audio_ms: int | None,
    finish: bool,
    frames: int,
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
        assert audio_out is not None and audio_ms is not None
        consumer = await asyncio.wait_for(client.follow(broadcast), timeout)
        catalog = await asyncio.wait_for(_catalog_with_media(consumer), timeout)

        id, video = next(iter(catalog.video.items()))

        media = await moq.media.TrackConsumer.subscribe(
            consumer, id, video, subscription=moq.Subscription(max_delay=MAX_DELAY)
        )

        total = 0

        async def drain() -> None:
            nonlocal total
            async for frame in media:
                if isinstance(frame, moq.media.Gap):
                    raise RuntimeError("media gap")
                if isinstance(frame, moq.media.End):
                    continue
                total += len(frame.payload)
                if total > 0:
                    return

        decodes = (_decode_audio(consumer, id, audio, audio_out, audio_ms) for id, audio in catalog.audio.items())
        await asyncio.wait_for(asyncio.gather(drain(), *decodes), timeout)

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
    parser.add_argument("--audio-out", help="subscribe: decode each audio rendition to PREFIX.<codec>.{txt,pcm}")
    parser.add_argument("--audio-ms", type=int, help="subscribe: how much audio to decode")
    parser.add_argument("--finish-ack", help="publisher completion FIFO; enables clean finish")
    parser.add_argument("--finish", action="store_true", help="verify a late tail and clean broadcast end")
    parser.add_argument("--frames", type=int, default=10, help="expected finish video frame count")
    args = parser.parse_args()
    if args.frames <= 0 or (args.finish and args.role != "subscribe") or (args.finish_ack and args.role != "publish"):
        parser.error("finish requires a subscriber, finish-ack a publisher, and frames must be positive")
    if args.role == "publish" and not args.tone:
        parser.error("publish needs --tone")
    if args.role == "subscribe" and not args.finish and (not args.audio_out or not args.audio_ms):
        parser.error("subscribe needs --audio-out and --audio-ms")

    # Warnings and errors from the Rust layer, such as a publication outside the grant.
    moq.log_level("warn")
    try:
        if args.role == "publish":
            tones = [int(hz) for hz in args.tone.split(",")]
            asyncio.run(
                publish(args.url, args.broadcast, args.version, args.grant, args.timeout, tones, args.finish_ack)
            )
        else:
            asyncio.run(
                subscribe(
                    args.url,
                    args.broadcast,
                    args.version,
                    args.grant,
                    args.timeout,
                    args.audio_out,
                    args.audio_ms,
                    args.finish,
                    args.frames,
                )
            )
    except KeyboardInterrupt:
        pass
    except (TimeoutError, asyncio.TimeoutError):
        print("error: timed out waiting for data", file=sys.stderr)
        sys.exit(1)
    except Exception as err:  # interop client: any failure is a failure
        print(f"error: {err}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
