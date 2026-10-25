# [S] Report upstream bugs found during the pre-cut run

## Goal

Each bug below is reported upstream, or confirmed already fixed there, and
our workaround links the report.

## Plan

Posting outside `moq-dev` and `kixelated` needs maintainer approval for each
report.

- **GStreamer `faad`:** it drops the first AAC frame and shifts timestamps by
  one frame, so the interop AAC phase check skips GStreamer
  ([moq-dev/moq#6742](https://github.com/moq-dev/moq/pull/6742)). Remove the
  skip once it's fixed.
- **retina 0.4.21:** when the server sends an RTSP request during playback,
  retina logs a warning, returns `Pending`, and goes idle until something else
  wakes it ([moq-dev/moq#6736](https://github.com/moq-dev/moq/pull/6736)).
- **mr-boxington:** the eviction found while landing
  [moq-dev/moq#6709](https://github.com/moq-dev/moq/pull/6709).

Public API: none. Wire: none.
