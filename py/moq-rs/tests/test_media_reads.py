"""Encoded media reads preserve the same records as decoded audio and video."""

from datetime import timedelta
from typing import Any, cast

from moq import media
from moq_ffi import MoqMediaFrame, MoqMediaRead


async def test_tagged_media_reads_preserve_frames_breaks_and_eof():
    payload = b"encoded packet"
    records = iter(
        [
            MoqMediaRead.FRAME(frame=MoqMediaFrame(payload=payload, timestamp_us=123_456, keyframe=True)),
            MoqMediaRead.GAP(timestamp_us=123_456, duration=timedelta(microseconds=78_901)),
            MoqMediaRead.END(end_us=202_357),
            MoqMediaRead.FRAME(frame=MoqMediaFrame(payload=b"resumed", timestamp_us=202_357, keyframe=False)),
            None,
        ]
    )

    class Inner:
        async def next(self):
            return next(records)

    reader = media.TrackConsumer(cast(Any, Inner()))
    assert await reader._next() == media.MediaFrame(payload, timedelta(microseconds=123_456), True)
    assert await reader._next() == media.Gap(timedelta(microseconds=123_456), timedelta(microseconds=78_901))
    assert await reader._next() == media.End(timedelta(microseconds=202_357))
    assert await reader._next() == media.MediaFrame(b"resumed", timedelta(microseconds=202_357), False)
    assert await reader._next() is None
