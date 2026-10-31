# [S] Decoding from a track's first packet keeps its start

## Goal

moq-audio decoding an Opus track from its first packet outputs everything but
the codec's pre-skip, so recordings keep their first 80 ms and a track shorter
than that decodes to something, in every binding.

## Plan

Decided 2026-10-30 (maintainer, `/quest-plan`): from the track's first packet,
drop only the OpusHead pre-skip, as the codec defines. A decoder that joins
mid-stream keeps today's 80 ms warmup, since it has no history to warm up
from. Found in [moq-dev/moq#7033](https://github.com/moq-dev/moq/pull/7033).

Test both cases: a short finished track decodes to its length minus
pre-skip, and a mid-stream join still drops its warmup.
