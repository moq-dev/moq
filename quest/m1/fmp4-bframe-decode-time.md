# [M] fMP4 fragments shorter than a GOP keep decode times increasing

## Goal

With B-frame video and a fragment duration shorter than a GOP, every fragment the fMP4 exporter writes has a `tfdt` at or after the previous fragment's end, so `moq export fmp4`, HLS packaging, and an unfinished `moq export mp4` file are valid fragmented MP4.

## Plan

Found by the agent on [moq-dev/moq#7357](https://github.com/moq-dev/moq/pull/7357): with `--fragment-duration` shorter than a GOP of B-frame video, the exporter writes fragment decode times that step backwards. The finished `export mp4` file is correct because it rebuilds its sample tables, but the fragmented output isn't.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). The fix is in how the exporter infers B-frame sample durations and decode times, which `export fmp4`, HLS packaging, and `export mp4` share; fix it there once rather than per consumer. Test with reordered video cut into fragments shorter than a GOP: decode times never decrease, presentation order and A/V timing are unchanged, and ffprobe reads every fragment without warnings.

Public API: none. Wire: none.
