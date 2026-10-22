# [XS] Fix the libav polyfill's planar copy in our fork

## Goal

The `@kixelated` fork of libavjs-webcodecs-polyfill converts interleaved
samples to planar from the right offset, a release of it is published, and
`@moq/hang` drops the copy patch that works around it.

## Plan

Found by [moq-dev/moq#6284](https://github.com/moq-dev/moq/pull/6284): the
polyfill garbles every channel after the first, so stereo Opus under Node, Bun,
or a browser without `AudioDecoder` lost its right channel. #6284 patches the
conversion in `Util.Libav.polyfill()` with a regression test; keep that test
when the patch goes.

Decided 2026-10-22 (maintainer): fix it in the fork only; post nothing upstream.

Public API: none. Wire: none.
