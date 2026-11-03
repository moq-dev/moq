"""Encoded media reads preserve the same records as decoded audio and video."""

from datetime import timedelta
from typing import Any, cast

from moq import Timestamp, media
from moq_ffi import MoqMediaFrame, MoqMediaRead, MoqTimestamp


async def test_tagged_media_reads_preserve_frames_breaks_and_eof():
    payload = b"encoded packet"
    first = MoqTimestamp(value=11_111, scale=90_000)
    end = MoqTimestamp(value=18_212, scale=90_000)
    records = iter(
        [
            MoqMediaRead.FRAME(frame=MoqMediaFrame(payload=payload, timestamp=first, keyframe=True)),
            MoqMediaRead.GAP(timestamp=first, duration=timedelta(microseconds=78_901)),
            MoqMediaRead.END(end=end),
            MoqMediaRead.FRAME(frame=MoqMediaFrame(payload=b"resumed", timestamp=end, keyframe=False)),
            None,
        ]
    )

    class Inner:
        async def next(self):
            return next(records)

    reader = media.TrackConsumer(cast(Any, Inner()))
    assert await reader._next() == media.MediaFrame(payload, Timestamp(11_111, 90_000), True)
    assert await reader._next() == media.Gap(Timestamp(11_111, 90_000), timedelta(microseconds=78_901))
    assert await reader._next() == media.End(Timestamp(18_212, 90_000))
    assert await reader._next() == media.MediaFrame(b"resumed", Timestamp(18_212, 90_000), False)
    assert await reader._next() is None
