# [XS] TS export stops trimming MPEG-2 open-GOP leading pictures

## Goal

The TS export forwards MPEG-2 video as whole PES without parsing picture
headers, unless a consumer that needs leading B-pictures trimmed is named.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
[moq-dev/moq#7365](https://github.com/moq-dev/moq/pull/7365) added
`Mpeg2Picture` to `rs/moq-mux/src/container/ts/export.rs`, a picture-header
parser that runs on every MPEG-2 video frame, plus 153 test lines. Its own
quest called it "Low priority: decoders usually skip these B-pictures", and no
consumer asked for it. Delete the parser, its call, and its tests; HEVC RASL
trimming, which the guide's evidence table also names, stays.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ delete it (rejected: name a consumer).

Public API: none. Wire: none.

